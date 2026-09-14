//! The Terraform registry protocol: provider and module versions, and a download.
//!
//! `v1/{providers|modules}/{…}/versions` is the listing the mirror index is
//! built from, `v1/{…}/{version}/download` (providers) carries the
//! `download_url` and its checksum files, and `v1/{…}/{version}` is the detail
//! document the `published_at` comes from.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::{artifact_bytes, delay, host, sha256_hex};
use crate::Args;

const VERSIONS: [&str; 3] = ["1.0.0", "1.1.0", "1.2.0"];

/// `GET /terraform/v1/providers/{namespace}/{type}/versions`
#[route(
    "/terraform/v1/providers/{namespace}/{ptype}/versions",
    method = "GET",
    method = "HEAD"
)]
async fn provider_versions(
    path: web::Path<(String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let _ = path;
    let versions: Vec<_> = VERSIONS
        .iter()
        .map(|v| {
            serde_json::json!({
                "version": v,
                "protocols": ["5.0"],
                "platforms": [{ "os": "linux", "arch": "amd64" }],
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::json!({ "versions": versions }).to_string())
}

/// `GET /terraform/v1/providers/{namespace}/{type}/{version}` — the detail document.
#[route(
    "/terraform/v1/providers/{namespace}/{ptype}/{version}",
    method = "GET",
    method = "HEAD"
)]
async fn provider_detail(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (namespace, ptype, version) = path.into_inner();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "id": format!("{namespace}/{ptype}/{version}"),
            "version": version,
            "published_at": "2024-01-01T00:00:00Z",
        })
        .to_string(),
    )
}

/// `GET /terraform/v1/providers/{ns}/{type}/{version}/download/{os}/{arch}`
#[route(
    "/terraform/v1/providers/{namespace}/{ptype}/{version}/download/{os}/{arch}",
    method = "GET",
    method = "HEAD"
)]
async fn provider_download(
    req: HttpRequest,
    path: web::Path<(String, String, String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (namespace, ptype, version, os, arch) = path.into_inner();
    let host = host(&req);
    let filename = format!("terraform-provider-{ptype}_{version}_{os}_{arch}.zip");
    let bytes = artifact_bytes(&filename, &version, args.artifact_size_kb * 1024);
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "protocols": ["5.0"],
            "os": os,
            "arch": arch,
            "filename": filename,
            "download_url": format!("http://{host}/terraform/files/{namespace}/{filename}"),
            "shasums_url": format!(
                "http://{host}/terraform/files/{namespace}/terraform-provider-{ptype}_{version}_SHA256SUMS"
            ),
            "shasum": sha256_hex(&bytes),
        })
        .to_string(),
    )
}

/// `GET /terraform/v1/modules/{ns}/{name}/{provider}/versions`
#[route(
    "/terraform/v1/modules/{namespace}/{name}/{provider}/versions",
    method = "GET",
    method = "HEAD"
)]
async fn module_versions(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (namespace, name, provider) = path.into_inner();
    let versions: Vec<_> = VERSIONS
        .iter()
        .map(|v| serde_json::json!({ "version": v }))
        .collect();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "modules": [{
                "source": format!("{namespace}/{name}/{provider}"),
                "versions": versions,
            }],
        })
        .to_string(),
    )
}

/// `GET /terraform/files/{namespace}/{filename}` — the provider zip or SHA256SUMS.
#[route(
    "/terraform/files/{namespace}/{filename}",
    method = "GET",
    method = "HEAD"
)]
async fn files(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_namespace, filename) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&filename, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(provider_versions)
        .service(provider_download)
        .service(module_versions)
        .service(provider_detail)
        .service(files);
}
