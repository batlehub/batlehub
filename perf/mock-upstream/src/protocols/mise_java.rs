//! mise's JVM catalogue: one metadata document under `/mise-java/jvm/`.
//!
//! Only the document: a row's archive is fetched from its vendor over
//! `https://` to a public address, which the proxy's redirect guard refuses
//! for this mock's loopback `http://` by design (RFC 0048 §15 Q2). The rows
//! name the default vendor hosts so the proxy keeps them; nothing fetches them.

use actix_web::{route, web, HttpResponse};

use crate::support::delay;
use crate::Args;

/// `GET /mise-java/jvm/ga/linux/x86_64.json` — the document mise reads first.
#[route("/mise-java/jvm/ga/linux/x86_64.json", method = "GET", method = "HEAD")]
async fn document(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let rows: Vec<_> = ["21.0.5+11.0.LTS", "21.0.4+7.0.LTS"]
        .iter()
        .map(|v| {
            serde_json::json!({
                "checksum": format!("sha256:{}", "ab".repeat(32)),
                "created_at": "2025-03-28T22:04:32.217430", "features": [],
                "file_type": "tar.gz", "image_type": "jdk", "java_version": v,
                "jvm_impl": "hotspot", "vendor": "temurin", "version": v,
                "url": format!("https://github.com/adoptium/temurin21-binaries/releases/download/jdk-{v}/OpenJDK21U-jdk_x64_linux_hotspot.tar.gz"),
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(rows).to_string())
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(document);
}
