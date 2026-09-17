//! Ansible Galaxy's collections API v3.
//!
//! Four documents, because the proxy reads four on the way to one artifact and
//! a mock that served only the listing would leave three of the soak's arms
//! answering `404` — a 404 passes a load generator's own check, which is how a
//! soak measures nothing and reports it as healthy.
//!
//! The versions listing is **paged**: `?limit=100` answers 100 entries with a
//! `links.next` naming the offset after them, because walking those pages is
//! what the proxy does on every cache fill and the cost of that walk is a thing
//! this soak exists to hold still.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay};
use crate::Args;

/// How many versions the mock collection has, and how many fit on a page —
/// upstream's own cap, which it applies whatever `?limit` asks for.
const VERSIONS: usize = 241;
const PAGE: usize = 100;

fn version_of(index: usize) -> String {
    format!("{}.{}.0", index / 20, index % 20)
}

/// `GET /galaxy/api/` — the discovery document `g_connect` reads first.
#[route("/galaxy/api/", method = "GET", method = "HEAD")]
async fn discovery(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"available_versions":{"v1":"v1/","v3":"v3/"}}"#)
}

/// `GET /galaxy/api/v3/collections/{ns}/{name}/` — re-read uncached on every
/// resolve, which is why it is its own arm.
#[route(
    "/galaxy/api/v3/collections/{ns}/{name}/",
    method = "GET",
    method = "HEAD"
)]
async fn collection(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (ns, name) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(
            serde_json::json!({
                "href": format!("/api/v3/collections/{ns}/{name}/"),
                "namespace": ns,
                "name": name,
                "deprecated": false,
                "versions_url": format!("/api/v3/collections/{ns}/{name}/versions/"),
                "highest_version": {
                    "href": format!("/api/v3/collections/{ns}/{name}/versions/{}/", version_of(VERSIONS - 1)),
                    "version": version_of(VERSIONS - 1),
                },
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
            })
            .to_string(),
        )
}

/// `GET /galaxy/api/v3/collections/{ns}/{name}/versions/` — one page of 100,
/// with the `links.next` the proxy follows.
#[route(
    "/galaxy/api/v3/collections/{ns}/{name}/versions/",
    method = "GET",
    method = "HEAD"
)]
async fn versions(
    path: web::Path<(String, String)>,
    query: web::Query<std::collections::HashMap<String, String>>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (ns, name) = path.into_inner();
    // A `HashMap` rather than a typed struct: this crate has no `serde` derive
    // dependency, and one query parameter does not justify adding one.
    let offset = query
        .get("offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(VERSIONS);
    let end = (offset + PAGE).min(VERSIONS);
    let data: Vec<_> = (offset..end)
        .map(|i| {
            let v = version_of(i);
            serde_json::json!({
                "version": v,
                "href": format!("/api/v3/collections/{ns}/{name}/versions/{v}/"),
                "created_at": "2020-01-02T00:00:00Z",
                "updated_at": "2020-01-02T00:00:00Z",
                "requires_ansible": ">=2.15.0",
                "marks": [],
            })
        })
        .collect();
    let next = (end < VERSIONS).then(|| {
        format!("/galaxy/api/v3/collections/{ns}/{name}/versions/?limit={PAGE}&offset={end}")
    });
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "meta": { "count": VERSIONS },
            "links": { "first": null, "previous": null, "next": next, "last": null },
            "data": data,
        })
        .to_string(),
    )
}

/// `GET /galaxy/api/v3/collections/{ns}/{name}/versions/{version}/` — the
/// document carrying `download_url` and `artifact.sha256`.
#[route(
    "/galaxy/api/v3/collections/{ns}/{name}/versions/{version}/",
    method = "GET",
    method = "HEAD"
)]
async fn version_detail(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (ns, name, version) = path.into_inner();
    let file = format!("{ns}-{name}-{version}.tar.gz");
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "version": version,
            "href": format!("/api/v3/collections/{ns}/{name}/versions/{version}/"),
            "created_at": "2020-01-02T00:00:00Z",
            "requires_ansible": ">=2.15.0",
            "artifact": { "filename": file, "sha256": null, "size": args.artifact_size_kb * 1024 },
            "collection": { "name": name, "href": format!("/api/v3/collections/{ns}/{name}/") },
            "namespace": { "name": ns },
            "download_url": format!("/galaxy/api/v3/artifacts/collections/{file}"),
            "metadata": { "dependencies": {} },
            "signatures": [],
        })
        .to_string(),
    )
}

/// `GET /galaxy/api/v3/artifacts/collections/{filename}` — the tarball.
///
/// No `sha256` in the document above, on purpose: the proxy checks the digest
/// only when the document carries one, and a mock that published a digest it
/// did not compute over these bytes would fail every fetch.
#[route(
    "/galaxy/api/v3/artifacts/collections/{filename}",
    method = "GET",
    method = "HEAD"
)]
async fn artifact(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let filename = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/gzip")
        .body(artifact_bytes(&filename, "galaxy", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(discovery)
        .service(versions)
        .service(version_detail)
        .service(collection)
        .service(artifact);
}
