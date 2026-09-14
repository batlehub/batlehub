//! The RubyGems compact index.
//!
//! Generated to order rather than stored, so a corpus size is a flag rather
//! than a fixture file: the L corpus's document is ~30 MB and does not belong
//! in git.
//!
//! The shape is the compact-index format Bundler reads and the one
//! `LocalRegistryService::get_rubygems_compact_versions` emits, down to the
//! `created_at` epoch and the per-gem MD5 — not because this mock's bytes are
//! checked, but because the proxy parses what it caches, and a document it
//! cannot parse would measure an error path.

use actix_web::{route, web, HttpResponse};

use crate::support::*;
use crate::Args;

/// `GET /gems/versions` — every gem in the registry, with its live versions.
#[route("/gems/versions", method = "GET", method = "HEAD")]
async fn gem_versions_doc(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let mut out = String::from("created_at: 1970-01-01T00:00:00Z\n---\n");
    for p in 0..args.gems {
        let name = format!("perf-gem-{p:07}");
        // Every tenth version yanked, matching `corpus-seed`: `/versions` drops
        // them and `/names` does not.
        let live: Vec<String> = (0..args.gem_versions)
            .filter(|v| v % 10 != 9)
            .map(|v| format!("{}.{}.{}", v / 100, (v / 10) % 10, v % 10))
            .collect();
        if live.is_empty() {
            continue;
        }
        out.push_str(&format!("{name} {} {:032x}\n", live.join(","), p as u128));
    }
    HttpResponse::Ok().content_type("text/plain").body(out)
}

/// `GET /gems/names` — the gem names alone.
#[route("/gems/names", method = "GET", method = "HEAD")]
async fn gem_names_doc(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let mut out = String::from("---\n");
    for p in 0..args.gems {
        out.push_str(&format!("perf-gem-{p:07}\n"));
    }
    HttpResponse::Ok().content_type("text/plain").body(out)
}

/// `GET /gems/info/{name}` — one gem's versions with their dependencies.
///
/// The third document Bundler reads, and the one it reads *per gem*, so a
/// resolve is one `/versions` plus one of these for every gem in the graph.
/// Generated to match `render_compact_info` in
/// `crates/core/src/services/local_registry/eco_rubygems.rs`, down to the
/// `|checksum:` separator: the proxy parses what it caches, and a document it
/// cannot parse would measure an error path.
#[route("/gems/info/{name}", method = "GET", method = "HEAD")]
async fn gem_info_doc(name: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let name = name.into_inner();
    let mut out = String::from("---\n");
    for v in 0..args.gem_versions {
        // Every tenth version is yanked upstream, matching `/versions` and
        // `corpus-seed`; a yanked version has no line here either.
        if v % 10 == 9 {
            continue;
        }
        let version = format!("{}.{}.{}", v / 100, (v / 10) % 10, v % 10);
        let checksum = sha256_hex(format!("{name}-{version}").as_bytes());
        out.push_str(&format!("{version} |checksum:{checksum}\n"));
    }
    HttpResponse::Ok().content_type("text/plain").body(out)
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(gem_versions_doc)
        .service(gem_names_doc)
        .service(gem_info_doc);
}
