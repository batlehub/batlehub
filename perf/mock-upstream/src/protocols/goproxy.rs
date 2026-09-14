//! The Go module proxy.
//!
//! Four routes and no cleverness: `@v/list` is newline-separated versions, the
//! `.info` is two fields, the `.mod` is a module line, and the `.zip` is bytes.
//! The Go *tool* is never here — `tests/heavy/closed_world.sh` owns that — so
//! what this has to be is parseable by the proxy's client, which is all the
//! soak's load needs.

use actix_web::{route, web, HttpResponse};

use crate::support::*;
use crate::Args;

/// `GET /go/{module}/@v/list`
#[route("/go/{module:.*}/@v/list", method = "GET", method = "HEAD")]
async fn go_list(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let _ = path;
    HttpResponse::Ok()
        .content_type("text/plain")
        .body("v1.0.0\nv1.1.0\nv1.2.0\n")
}

/// `GET /go/{module}/@v/{file}` — `.info`, `.mod` or `.zip`.
#[route("/go/{module:.*}/@v/{file}", method = "GET", method = "HEAD")]
async fn go_meta(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (module, file) = path.into_inner();
    let version = file
        .rsplit_once('.')
        .map(|(v, _)| v.to_owned())
        .unwrap_or_else(|| "v1.2.0".to_owned());
    if file.ends_with(".info") {
        return HttpResponse::Ok().content_type("application/json").body(
            serde_json::json!({ "Version": version, "Time": "2024-01-01T00:00:00Z" }).to_string(),
        );
    }
    if file.ends_with(".mod") {
        return HttpResponse::Ok()
            .content_type("text/plain")
            .body(format!("module {module}\n\ngo 1.21\n"));
    }
    HttpResponse::Ok()
        .content_type("application/zip")
        .body(artifact_bytes(
            &module,
            &version,
            args.artifact_size_kb * 1024,
        ))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(go_list).service(go_meta);
}
