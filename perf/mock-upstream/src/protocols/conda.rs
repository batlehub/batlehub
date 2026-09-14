//! Conda: `repodata.json` and the packages it indexes.
//!
//! **Small on purpose, and that is the caveat.** A real channel's `repodata`
//! is hundreds of megabytes — `noarch` plus one platform on conda-forge is
//! ~424 MiB, and parsing it as a `serde_json::Value` costs ~11.5 GB, which is
//! why the proxy has a streaming filter and a sharded index at all. This mock
//! serves a few hundred entries, so the soak exercises the *code path* —
//! fetch, parse, filter, rewrite, cache — under constant load, and says
//! nothing about the memory profile at channel scale. That measurement has its
//! own instrument (`tests/heavy/closed_world.sh`'s conda phase, and the sharded
//! index the size drove).

use actix_web::{route, web, HttpResponse};

use crate::support::*;
use crate::Args;

/// How many packages the generated `repodata.json` names. Enough that the
/// parse is not free, small enough that the file stays in the hundreds of KB.
const PACKAGES: usize = 200;

fn filename_for(n: usize) -> String {
    format!("perf-conda-{n:04}-1.0.0-py311_0.conda")
}

/// `GET /conda/{platform}/repodata.json`
#[route("/conda/{platform}/repodata.json", method = "GET", method = "HEAD")]
async fn repodata(platform: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let platform = platform.into_inner();
    let mut packages_conda = serde_json::Map::new();
    for n in 0..PACKAGES {
        let filename = filename_for(n);
        let bytes = artifact_bytes(&filename, "", args.artifact_size_kb * 1024);
        packages_conda.insert(
            filename,
            serde_json::json!({
                "name": format!("perf-conda-{n:04}"),
                "version": "1.0.0",
                "build": "py311_0",
                "build_number": 0,
                "subdir": platform,
                "sha256": sha256_hex(&bytes),
                "size": bytes.len(),
                "timestamp": 1_704_067_200_000i64,
                "depends": [],
            }),
        );
    }
    let body = serde_json::json!({
        "info": { "subdir": platform },
        "packages": {},
        "packages.conda": packages_conda,
        "repodata_version": 1,
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// `GET /conda/channeldata.json` — the channel-level document.
#[route("/conda/channeldata.json", method = "GET", method = "HEAD")]
async fn channeldata() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::json!({ "channeldata_version": 1, "packages": {} }).to_string())
}

/// `GET /conda/{platform}/{filename}` — the package `repodata` indexed.
#[route("/conda/{platform}/{filename}", method = "GET", method = "HEAD")]
async fn package(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_platform, filename) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&filename, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(repodata).service(channeldata).service(package);
}
