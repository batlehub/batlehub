//! NuGet V3: the service index, the flat container and a `.nupkg`.
//!
//! `v3-flatcontainer/{id}/index.json` is `{"versions":[…]}` — the document the
//! client reads to learn what exists — and `{id}/{version}/{file}` is the
//! package. The registration index is served too because the proxy consults it
//! for the published date when the flat index alone cannot answer.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::*;
use crate::Args;

const VERSIONS: [&str; 3] = ["1.0.0", "1.1.0", "1.2.0"];

/// `GET /nuget/v3/index.json` — the service index, which names the others.
#[route("/nuget/v3/index.json", method = "GET", method = "HEAD")]
async fn service_index(req: HttpRequest) -> HttpResponse {
    let host = host(&req);
    let body = serde_json::json!({
        "version": "3.0.0",
        "resources": [
            {
                "@id": format!("http://{host}/nuget/v3-flatcontainer/"),
                "@type": "PackageBaseAddress/3.0.0",
            },
            {
                "@id": format!("http://{host}/nuget/v3/registration5/"),
                "@type": "RegistrationsBaseUrl/3.6.0",
            },
        ],
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// `GET /nuget/v3-flatcontainer/{id}/index.json`
#[route(
    "/nuget/v3-flatcontainer/{id}/index.json",
    method = "GET",
    method = "HEAD"
)]
async fn flat_index(id: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let _ = id;
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::json!({ "versions": VERSIONS }).to_string())
}

/// `GET /nuget/v3-flatcontainer/{id}/{version}/{filename}` — the `.nupkg`.
#[route(
    "/nuget/v3-flatcontainer/{id}/{version}/{filename}",
    method = "GET",
    method = "HEAD"
)]
async fn nupkg(path: web::Path<(String, String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (id, version, _filename) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&id, &version, args.artifact_size_kb * 1024))
}

/// `GET /nuget/v3/registration5/{id}/index.json` — the dates the flat index has not got.
#[route(
    "/nuget/v3/registration5/{id}/index.json",
    method = "GET",
    method = "HEAD"
)]
async fn registration_index(
    req: HttpRequest,
    id: web::Path<String>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let id = id.into_inner();
    let host = host(&req);
    let items: Vec<_> = VERSIONS
        .iter()
        .map(|v| {
            serde_json::json!({
                "catalogEntry": {
                    "id": id,
                    "version": v,
                    "published": "2024-01-01T00:00:00+00:00",
                    "packageContent": format!(
                        "http://{host}/nuget/v3-flatcontainer/{id}/{v}/{id}.{v}.nupkg"
                    ),
                },
            })
        })
        .collect();
    let body = serde_json::json!({
        "count": 1,
        "items": [{ "count": items.len(), "items": items }],
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(service_index)
        .service(flat_index)
        .service(nupkg)
        .service(registration_index);
}
