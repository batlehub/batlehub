//! The Rust release tree: `manifests.txt`, a channel manifest and the files.
//!
//! `manifests.txt` is how a bare version (`1.90.0`) is turned into the dated
//! directory its files live under, so it is the document that makes every other
//! route reachable. The channel manifest is TOML and only has to name the
//! release; the artifacts themselves are addressed by path under `dist/{date}/`.
//!
//! `HEAD` matters here and nowhere else in this mock: the client asks whether an
//! artifact exists before streaming it, precisely so a yes/no question does not
//! read a 200 MB tarball. actix answers `HEAD` from the `#[get]` handler, so
//! the routes below serve both.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay};
use crate::Args;

/// The one dated directory this mock publishes, and the release in it.
const DATE: &str = "2024-01-01";
const VERSION: &str = "1.90.0";
const RUSTUP_VERSION: &str = "1.28.0";

/// `GET /rustup/manifests.txt` — `{date}/channel-rust-{version}.toml` per line.
#[route("/rustup/manifests.txt", method = "GET", method = "HEAD")]
async fn manifests(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    HttpResponse::Ok().content_type("text/plain").body(format!(
        "static.rust-lang.org/dist/{DATE}/channel-rust-{VERSION}.toml\n\
         static.rust-lang.org/dist/{DATE}/channel-rust-stable.toml\n"
    ))
}

/// `GET /rustup/dist/channel-rust-{channel}.toml` — the undated manifest.
#[route(
    "/rustup/dist/channel-rust-{channel}.toml",
    method = "GET",
    method = "HEAD"
)]
async fn channel_manifest(channel: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    HttpResponse::Ok()
        .content_type("text/plain")
        .body(manifest_toml(&channel.into_inner()))
}

/// `GET /rustup/dist/{date}/{file}` — the dated manifest, or an artifact.
#[route("/rustup/dist/{date}/{file}", method = "GET", method = "HEAD")]
async fn dated(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (date, file) = path.into_inner();
    if let Some(channel) = file
        .strip_prefix("channel-rust-")
        .and_then(|f| f.strip_suffix(".toml"))
    {
        return HttpResponse::Ok()
            .content_type("text/plain")
            .body(manifest_toml(channel));
    }
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&file, &date, args.artifact_size_kb * 1024))
}

/// `GET /rustup/rustup/release-stable.toml` — the installer's own version.
#[route("/rustup/rustup/release-stable.toml", method = "GET", method = "HEAD")]
async fn release_stable() -> HttpResponse {
    HttpResponse::Ok().content_type("text/plain").body(format!(
        "schema-version = '1'\nversion = '{RUSTUP_VERSION}'\n"
    ))
}

/// `GET /rustup/rustup/archive/{version}/{triple}/{file}` — an installer build.
#[route(
    "/rustup/rustup/archive/{version}/{triple}/{file}",
    method = "GET",
    method = "HEAD"
)]
async fn rustup_archive(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (version, triple, file) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &format!("{triple}/{file}"),
            &version,
            args.artifact_size_kb * 1024,
        ))
}

fn manifest_toml(channel: &str) -> String {
    format!(
        "manifest-version = '2'\n\
         date = '{DATE}'\n\n\
         [pkg.rust]\n\
         version = '{VERSION} ({channel} {DATE})'\n"
    )
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(manifests)
        .service(channel_manifest)
        .service(dated)
        .service(release_stable)
        .service(rustup_archive);
}
