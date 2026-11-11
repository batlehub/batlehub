//! mise's versions host: one TOML version list per tool (RFC 0043).
//!
//! The proxy filters the list and serves it from its metadata cache, so the
//! soak measures the filter and the cache, not this mock.

use actix_web::{route, web, HttpResponse};

use crate::support::delay;
use crate::Args;

/// `GET /mise-versions/data/{tool}.toml` — the list mise reads before any
/// backend's own.
#[route("/mise-versions/data/{file}", method = "GET", method = "HEAD")]
async fn version_list(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    if !path.ends_with(".toml") {
        return HttpResponse::NotFound().finish();
    }
    HttpResponse::Ok().content_type("application/toml").body(
        "[versions]\n\
         \"1.7.1\" = { created_at = 2023-12-13T19:56:17.000Z }\n\
         \"1.8.0\" = { created_at = 2025-06-01T06:05:01.000Z }\n\
         \"1.8.1\" = { created_at = 2025-07-01T11:50:06.000Z }\n",
    )
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(version_list);
}
