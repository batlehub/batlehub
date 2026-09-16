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

/// The dated directory this mock publishes, and how many releases live in it.
///
/// **Eight, because the soak's `rustup_component` arm walks eight.** That arm
/// asks for `rust-std-1.9{0..7}.0-…` under this date, and a version that
/// `manifests.txt` does not list has no dated directory to resolve to: the
/// proxy answers `404`, and the load's own check — `status < 500` — passes it.
/// Measured 2026-09-16 with `--out json`: **34 of that arm's 38 requests were
/// 404s**, 1.13 % of the whole mix, and `perf-rustup` came out of the soak
/// looking like the cheapest registry in the ranking because most of its
/// requests were failing rather than because it was cheap.
///
/// So the two numbers have to agree: this one and `space` on the arm in
/// `perf/k6/soak_arms.js`. The pre-flight cannot catch a disagreement — it asks
/// each arm once, at the first point of its space, which is the one that works.
const DATE: &str = "2024-01-01";
const VERSION_COUNT: usize = 8;
const RUSTUP_VERSION: &str = "1.28.0";

/// `1.90.0` … `1.97.0` — the releases this mock publishes, in the order the
/// dated directory lists them.
fn versions() -> impl Iterator<Item = String> {
    (0..VERSION_COUNT).map(|n| format!("1.9{n}.0"))
}

/// `GET /rustup/manifests.txt` — `{date}/channel-rust-{version}.toml` per line.
#[route("/rustup/manifests.txt", method = "GET", method = "HEAD")]
async fn manifests(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let mut body = String::new();
    for version in versions() {
        body.push_str(&format!(
            "static.rust-lang.org/dist/{DATE}/channel-rust-{version}.toml\n"
        ));
    }
    body.push_str(&format!(
        "static.rust-lang.org/dist/{DATE}/channel-rust-stable.toml\n"
    ));
    HttpResponse::Ok().content_type("text/plain").body(body)
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
    // A version channel names itself; a named one (`stable`) resolves to the
    // newest release this mock has, which is what the real tree does.
    let version = if versions().any(|v| v == channel) {
        channel.to_owned()
    } else {
        versions().last().unwrap_or_else(|| "1.90.0".to_owned())
    };
    format!(
        "manifest-version = '2'\n\
         date = '{DATE}'\n\n\
         [pkg.rust]\n\
         version = '{version} ({channel} {DATE})'\n"
    )
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(manifests)
        .service(channel_manifest)
        .service(dated)
        .service(release_stable)
        .service(rustup_archive);
}
