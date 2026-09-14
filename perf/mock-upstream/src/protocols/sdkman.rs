//! SDKMAN: the candidate API and the broker that serves the archives.
//!
//! Two bases, because SDKMAN has two: the API answers text documents about
//! candidates and versions, and the *broker* is a separate host that redirects
//! a download to wherever the vendor keeps it. The proxy treats them as two
//! upstreams and only sends credentials to the two it was configured with, so
//! the mock serves both from one process under two path prefixes.
//!
//! The API base carries SDKMAN's path version (`/2`), because the server warns
//! when it does not — the candidates API is versioned in the path and a URL
//! without it is a typo that answers 404 at `sdk list` time.
//!
//! The broker's real answer is a cross-host `302`. This one answers the bytes
//! directly: a redirect to a vendor CDN is not something a mock can honour
//! without becoming that CDN too, and the redirect follower has its own test
//! (`tests/heavy/sdkman.sh`).

use actix_web::{route, web, HttpResponse};

use crate::support::*;
use crate::Args;

const CANDIDATES: [&str; 3] = ["java", "maven", "gradle"];
const VERSIONS: [&str; 3] = ["21.0.1-tem", "21.0.2-tem", "21.0.3-tem"];

const TEXT: &str = "text/plain";

/// `GET /sdkman/api/2/candidates/list` — the rendered table SDKMAN prints.
#[route("/sdkman/api/2/candidates/list", method = "GET", method = "HEAD")]
async fn candidates_list(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let mut body = String::from("=============================\nAvailable Candidates\n=====\n");
    for c in CANDIDATES {
        body.push_str(&format!("{c}\n"));
    }
    HttpResponse::Ok().content_type(TEXT).body(body)
}

/// `GET /sdkman/api/2/candidates/all` — the comma-separated machine-readable list.
#[route("/sdkman/api/2/candidates/all", method = "GET", method = "HEAD")]
async fn candidates_all(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    HttpResponse::Ok()
        .content_type(TEXT)
        .body(CANDIDATES.join(","))
}

/// `GET /sdkman/api/2/candidates/default/{candidate}`
#[route(
    "/sdkman/api/2/candidates/default/{candidate}",
    method = "GET",
    method = "HEAD"
)]
async fn candidate_default(candidate: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let _ = candidate;
    HttpResponse::Ok().content_type(TEXT).body(VERSIONS[0])
}

/// `GET /sdkman/api/2/candidates/{candidate}/{platform}/versions/all`
#[route(
    "/sdkman/api/2/candidates/{candidate}/{platform}/versions/all",
    method = "GET",
    method = "HEAD"
)]
async fn versions_all(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let _ = path;
    HttpResponse::Ok()
        .content_type(TEXT)
        .body(VERSIONS.join(","))
}

/// `GET /sdkman/api/2/candidates/{candidate}/{platform}/versions/list`
///
/// The real API answers `400` without `current=` and `installed=`, so the
/// client always sends the pair. Answering here regardless of the query keeps
/// the mock from encoding a refusal the proxy is not being measured on.
#[route(
    "/sdkman/api/2/candidates/{candidate}/{platform}/versions/list",
    method = "GET",
    method = "HEAD"
)]
async fn versions_list(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (candidate, _platform) = path.into_inner();
    let mut body = format!("Available {candidate} Versions\n================\n");
    for v in VERSIONS {
        body.push_str(&format!(" {v}\n"));
    }
    HttpResponse::Ok().content_type(TEXT).body(body)
}

/// `GET /sdkman/api/2/candidates/validate/{candidate}/{version}/{platform}`
#[route(
    "/sdkman/api/2/candidates/validate/{candidate}/{version}/{platform}",
    method = "GET",
    method = "HEAD"
)]
async fn validate(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_candidate, version, _platform) = path.into_inner();
    let answer = if VERSIONS.contains(&version.as_str()) {
        "valid"
    } else {
        "invalid"
    };
    HttpResponse::Ok().content_type(TEXT).body(answer)
}

/// `GET /sdkman/broker/download/{candidate}/{version}/{platform}` — the archive.
#[route(
    "/sdkman/broker/download/{candidate}/{version}/{platform}",
    method = "GET",
    method = "HEAD"
)]
async fn broker_download(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (candidate, version, platform) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/zip")
        .body(artifact_bytes(
            &format!("{candidate}/{platform}"),
            &version,
            args.artifact_size_kb * 1024,
        ))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(candidates_list)
        .service(candidates_all)
        .service(candidate_default)
        .service(versions_all)
        .service(versions_list)
        .service(validate)
        .service(broker_download);
}
