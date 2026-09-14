//! Mock upstream registry for BatleHub's performance and soak suites.
//!
//! One process that speaks every registry protocol the soak drives, so a run
//! offers thousands of requests a minute for an hour without pointing any of
//! them at somebody else's registry.
//!
//! **What this is and is not.** Each module here answers what the *proxy's*
//! client parses — the document shapes, the digests it verifies, the
//! self-referential URLs it rewrites — and no more. It is not a registry a real
//! package manager could install from, and it is not trying to be: that is
//! `tests/heavy/closed_world.sh`, which drives the actual clients. The division
//! is deliberate, and it is what makes covering twenty-odd kinds here cost a
//! few routes each instead of a fixture tree.
//!
//! What a soak needs from an upstream is narrow and all three parts matter:
//!
//!   - **parseable** — a document the proxy cannot read measures an error path;
//!   - **verifiable** — every digest a format names is computed from the bytes
//!     that will be served, because the proxy checks them (`support.rs` says
//!     what happened when they were random);
//!   - **stationary** — the same coordinate answers the same bytes forever, so
//!     a cache hit and a cache miss are not distinguishable by content.

use actix_web::{get, web, App, HttpResponse, HttpServer};
use clap::Parser;

mod protocols;
mod support;

#[derive(Parser, Clone)]
#[command(about = "Mock upstream registry for BatleHub performance tests")]
struct Args {
    #[arg(long, default_value = "9999")]
    port: u16,

    /// Simulated upstream latency in milliseconds
    #[arg(long, default_value = "0")]
    delay_ms: u64,

    /// Fake artifact size in kilobytes
    #[arg(long, default_value = "512")]
    artifact_size_kb: usize,

    /// Packages in the generated RubyGems compact index, and versions of each.
    ///
    /// RFC 0015 §11.7 arm 1 is "today — unfiltered, shared cache", which on this
    /// server is the *proxy* path: the upstream document is cached under an
    /// identity-blind key and served to everyone. That arm needs an upstream
    /// document of the same size as the local corpus arm 2 builds, or the two
    /// numbers describe different documents and comparing them says nothing.
    /// These two flags mirror `corpus-seed --size`.
    #[arg(long, default_value = "1000")]
    gems: usize,

    #[arg(long, default_value = "5")]
    gem_versions: usize,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();
    println!(
        "mock-upstream listening on :{} delay={}ms artifact={}KB",
        args.port, args.delay_ms, args.artifact_size_kb
    );

    let args = web::Data::new(args);
    let port = args.port;

    HttpServer::new(move || {
        App::new()
            .app_data(args.clone())
            .service(health)
            .configure(protocols::configure)
    })
    .bind(("0.0.0.0", port))?
    .run()
    .await
}

/// `GET /health` — what the harness waits for before starting the load.
#[get("/health")]
async fn health() -> HttpResponse {
    HttpResponse::Ok().body("ok")
}
