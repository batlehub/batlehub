//! Conda: `repodata.json` and the packages it indexes.
//!
//! **This is the one protocol here whose *size* is the measurement.** A real
//! channel's `repodata` is hundreds of megabytes — `noarch` plus one platform
//! on conda-forge is ~424 MiB across ~1.4 million entries, and parsing that as
//! a `serde_json::Value` costs ~11.5 GB, which is why the proxy has a streaming
//! filter (`blocking::conda_stream`) and a sharded index at all.
//!
//! So the index is generated to order: `--conda-packages` says how many entries
//! it names, and the soak's default of 200 keeps that arm cheap while
//! `perf:conda:upstream` raises it until the document is channel-shaped. The
//! scenario that needs it is `12_conda_filter.js`, which measures what the
//! filtering path costs in RAM at a size where the answer is not zero.
//!
//! Two things follow from generating a large document:
//!
//!   * **It is built once per platform and cached.** At 200 000 entries the
//!     build hashes a couple of hundred megabytes; doing that per request would
//!     make the mock the bottleneck and measure the mock.
//!   * **The entries' artifacts are small by default** (`--conda-artifact-kb`,
//!     defaulting to `--artifact-size-kb`). The digest in the index is the real
//!     sha256 of the bytes the package route will serve — the proxy verifies it
//!     — so every entry costs a hash of that size at build time. At channel
//!     scale, 512 KB an entry is 700 GB of hashing; 1 KB is 1.4 GB.

use std::collections::HashMap;
use std::sync::RwLock;

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay, sha256_hex};
use crate::Args;

/// One platform's generated index, kept so it is built once.
///
/// Keyed by platform because `subdir` is inside the document: two platforms are
/// two different documents, and a client that asked for `linux-64` must not be
/// told it is looking at `noarch`.
///
/// The value is `Bytes` and not `String`, so serving it is a refcount bump.
/// A channel-sized index copied per request would put the *mock's* allocator in
/// the measurement, which is the one thing this scenario must not measure.
#[derive(Default)]
pub struct RepodataCache(RwLock<HashMap<String, web::Bytes>>);

impl RepodataCache {
    /// The index for one subdir, in one encoding, built on first ask.
    ///
    /// The compressed form is built from the plain one and cached beside it:
    /// a channel publishes both, and the proxy picks whichever the route it is
    /// answering needs.
    fn get_or_build(&self, platform: &str, encoding: Encoding, args: &Args) -> web::Bytes {
        let key = format!("{platform}{}", encoding.suffix());
        if let Some(doc) = self.0.read().expect("repodata cache poisoned").get(&key) {
            return doc.clone();
        }
        let plain = build_repodata(platform, args);
        let built = match encoding {
            Encoding::Plain => web::Bytes::from(plain),
            // Level 3 is what conda-forge publishes at, and the ratio is what
            // decides how much the proxy buffers: 424 MiB of JSON is a 55 MiB
            // `.zst`, and it is the compressed size the filtering path holds.
            Encoding::Zstd => web::Bytes::from(
                zstd::encode_all(plain.as_bytes(), 3).expect("zstd encoding a repodata"),
            ),
        };
        self.0
            .write()
            .expect("repodata cache poisoned")
            .insert(key, built.clone());
        built
    }
}

/// The encodings this mock publishes the index in.
#[derive(Clone, Copy)]
enum Encoding {
    Plain,
    Zstd,
}

impl Encoding {
    fn suffix(self) -> &'static str {
        match self {
            Encoding::Plain => "",
            Encoding::Zstd => ".zst",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Encoding::Plain => "application/json",
            Encoding::Zstd => "application/zstd",
        }
    }
}

fn filename_for(n: usize) -> String {
    format!("perf-conda-{n:04}-1.0.0-py311_0.conda")
}

/// The size of the package bytes an entry's digest is computed over.
fn artifact_kb(args: &Args) -> usize {
    args.conda_artifact_kb.unwrap_or(args.artifact_size_kb)
}

/// Write the document out by hand rather than through `serde_json::Value`.
///
/// The same reason the proxy's filter streams: a `Value` of a channel-sized
/// index is gigabytes. Here the entries are uniform and the only value that is
/// not a literal is the filename, which goes through `serde_json` so escaping
/// stays its business rather than this function's.
fn build_repodata(platform: &str, args: &Args) -> String {
    let count = args.conda_packages;
    let size = artifact_kb(args) * 1024;
    // ~260 bytes an entry, measured on the shape below; a re-allocation on a
    // 350 MB string is the difference between seconds and tens of seconds.
    let mut out = String::with_capacity(count * 280 + 256);
    out.push_str("{\"info\":{\"subdir\":");
    out.push_str(&serde_json::to_string(platform).expect("a string serialises"));
    out.push_str("},\"packages\":{},\"packages.conda\":{");
    for n in 0..count {
        let filename = filename_for(n);
        let bytes = artifact_bytes(&filename, "", size);
        if n > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(&filename).expect("a string serialises"));
        out.push(':');
        out.push_str(&format!(
            "{{\"name\":\"perf-conda-{n:04}\",\"version\":\"1.0.0\",\"build\":\"py311_0\",\
             \"build_number\":0,\"subdir\":{subdir},\"sha256\":\"{sha}\",\"size\":{size},\
             \"timestamp\":1704067200000,\"depends\":[]}}",
            subdir = serde_json::to_string(platform).expect("a string serialises"),
            sha = sha256_hex(&bytes),
            size = bytes.len(),
        ));
    }
    out.push_str("},\"repodata_version\":1}");
    out
}

async fn serve_index(
    platform: String,
    encoding: Encoding,
    args: web::Data<Args>,
    cache: web::Data<RepodataCache>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    // Off the async runtime: building a channel-sized index is seconds of CPU,
    // and it would otherwise block every other request the soak has in flight.
    let args = args.into_inner();
    let cache = cache.into_inner();
    let doc = web::block(move || cache.get_or_build(&platform, encoding, &args))
        .await
        .expect("repodata build panicked");
    HttpResponse::Ok()
        .content_type(encoding.content_type())
        .body(doc)
}

/// `GET /conda/{platform}/repodata.json`
#[route("/conda/{platform}/repodata.json", method = "GET", method = "HEAD")]
async fn repodata(
    platform: web::Path<String>,
    args: web::Data<Args>,
    cache: web::Data<RepodataCache>,
) -> HttpResponse {
    serve_index(platform.into_inner(), Encoding::Plain, args, cache).await
}

/// `GET /conda/{platform}/repodata.json.zst` — what conda 23.x asks for first.
///
/// Its presence is what decides which of the proxy's two filtering paths runs:
/// with a compressed index to stream, blocked entries are taken out one at a
/// time (`blocking::conda_stream`); without one, the proxy builds the document
/// as a `serde_json::Value` and filters that. Scenario 12 measures both, which
/// it can only do because this route exists.
#[route("/conda/{platform}/repodata.json.zst", method = "GET", method = "HEAD")]
async fn repodata_zst(
    platform: web::Path<String>,
    args: web::Data<Args>,
    cache: web::Data<RepodataCache>,
) -> HttpResponse {
    serve_index(platform.into_inner(), Encoding::Zstd, args, cache).await
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
        .body(artifact_bytes(&filename, "", artifact_kb(&args) * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    // `repodata_zst` before `package`: the package route's `{filename}` would
    // otherwise swallow `repodata.json.zst` and answer it with artifact bytes.
    cfg.service(repodata)
        .service(repodata_zst)
        .service(channeldata)
        .service(package);
}
