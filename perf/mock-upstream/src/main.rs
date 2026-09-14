use actix_web::{get, web, App, HttpRequest, HttpResponse, HttpServer};
use clap::Parser;
use sha1::{Digest, Sha1};
use std::time::Duration;

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
            .service(npm_packument)
            .service(npm_tarball)
            .service(cargo_download)
            .service(cargo_index)
            .service(gem_versions_doc)
            .service(gem_names_doc)
            .service(gem_info_doc)
    })
    .bind(("0.0.0.0", port))?
    .run()
    .await
}

// ── health ────────────────────────────────────────────────────────────────────

#[get("/health")]
async fn health() -> HttpResponse {
    HttpResponse::Ok().body("ok")
}

// ── npm ───────────────────────────────────────────────────────────────────────

/// npm packument: GET /{name}
/// Serves a minimal packument with a single version whose tarball points back
/// to this mock server so the proxy fetches it from here too.
#[get("/npm/{name}")]
async fn npm_packument(
    req: HttpRequest,
    name: web::Path<String>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;

    // `disallowed_methods` forbids this in the server, where believing
    // `X-Forwarded-Host` from any peer decides the URLs it advertises. Here it
    // is the correct call and the only one available: this mock has to write
    // self-referential URLs (a packument's tarball, the sparse index's `dl`)
    // that point back at whatever address the proxy reached it on, and it is a
    // test double on loopback with nothing to spoof. Allowed with the reason,
    // rather than left as a lint nobody runs — this crate is its own workspace,
    // so `cargo clippy --workspace` at the root never sees it.
    #[allow(clippy::disallowed_methods)]
    let host = req.connection_info().host().to_string();
    let pkg = name.into_inner();
    let version = "1.0.0";
    let tarball_url = format!("http://{}/npm/{}/-/{}-{}.tgz", host, pkg, pkg, version);
    // The digest of the bytes `npm_tarball` will serve for this coordinate,
    // computed rather than invented — see `artifact_bytes`.
    let shasum = sha1_hex(&artifact_bytes(&pkg, version, args.artifact_size_kb * 1024));

    let body = serde_json::json!({
        "name": pkg,
        "dist-tags": { "latest": version },
        "versions": {
            version: {
                "name": pkg,
                "version": version,
                "description": "mock package for perf tests",
                "dist": {
                    "tarball": tarball_url,
                    "shasum": shasum
                }
            }
        },
        "time": {
            version: "2024-01-01T00:00:00.000Z"
        }
    });

    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// npm tarball: GET /{name}/-/{filename}.tgz
///
/// The bytes are derived from the coordinate, so two fetches of the same
/// tarball are the same tarball and both match the `shasum` the packument
/// advertised. See `artifact_bytes`.
#[get("/npm/{name}/-/{filename}")]
async fn npm_tarball(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (name, _filename) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&name, "1.0.0", args.artifact_size_kb * 1024))
}

// ── cargo ─────────────────────────────────────────────────────────────────────

/// Sparse cargo index config: GET /cargo/config.json
#[get("/cargo/config.json")]
async fn cargo_index(req: HttpRequest) -> HttpResponse {
    // `disallowed_methods` forbids this in the server, where believing
    // `X-Forwarded-Host` from any peer decides the URLs it advertises. Here it
    // is the correct call and the only one available: this mock has to write
    // self-referential URLs (a packument's tarball, the sparse index's `dl`)
    // that point back at whatever address the proxy reached it on, and it is a
    // test double on loopback with nothing to spoof. Allowed with the reason,
    // rather than left as a lint nobody runs — this crate is its own workspace,
    // so `cargo clippy --workspace` at the root never sees it.
    #[allow(clippy::disallowed_methods)]
    let host = req.connection_info().host().to_string();
    let body = serde_json::json!({
        "dl": format!("http://{}/cargo/{{crate}}/{{version}}/download", host),
        "api": format!("http://{}/cargo", host)
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// Cargo crate download: GET /cargo/{name}/{version}/download
#[get("/cargo/{name}/{version}/download")]
async fn cargo_download(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (name, version) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &name,
            &version,
            args.artifact_size_kb * 1024,
        ))
}

// ── rubygems compact index ────────────────────────────────────────────────────
//
// Generated to order rather than stored, so a corpus size is a flag rather than
// a fixture file: the L corpus's document is ~30 MB and does not belong in git.
//
// The shape is the compact-index format Bundler reads and the one
// `LocalRegistryService::get_rubygems_compact_versions` emits, down to the
// `created_at` epoch and the per-gem MD5 — not because this mock's bytes are
// checked, but because the proxy parses what it caches, and a document it
// cannot parse would measure an error path.

/// `GET /gems/versions` — every gem in the registry, with its live versions.
#[get("/gems/versions")]
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
#[get("/gems/names")]
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
#[get("/gems/info/{name}")]
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
        let checksum = sha1_hex(format!("{name}-{version}").as_bytes());
        out.push_str(&format!("{version} |checksum:{checksum}\n"));
    }
    HttpResponse::Ok().content_type("text/plain").body(out)
}

// ── helpers ───────────────────────────────────────────────────────────────────

async fn delay(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// An artifact's bytes: incompressible, and **the same every time** for a
/// coordinate.
///
/// It used to be `rand::thread_rng()`, which made every fetch of the same
/// tarball a different file. That is wrong twice over. The obvious way is that
/// the packument cannot then advertise a digest, and the proxy verifies the
/// one it is given: every artifact read answered 502 and every scenario that
/// checked for a 200 had been failing since integrity checking landed. The
/// subtler way is that a cache is supposed to return the bytes it stored, and
/// an upstream whose answer changes per request makes "the same artifact"
/// meaningless — a cache hit and a cache miss would be distinguishable by
/// content, which is not a property any real registry has.
///
/// An xorshift seeded from the coordinate rather than a hash chain: this runs
/// on the packument path too, where it is the thing being timed, and the bytes
/// only have to be reproducible and not compress away — not unpredictable.
fn artifact_bytes(name: &str, version: &str, size: usize) -> Vec<u8> {
    let mut state = 0xcbf2_9ce4_8422_2325u64; // FNV-1a offset basis
    for byte in name
        .bytes()
        .chain(b"@".iter().copied())
        .chain(version.bytes())
    {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x1000_0000_01b3);
    }
    state |= 1; // xorshift is degenerate from zero

    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        buf.extend_from_slice(&state.to_le_bytes());
    }
    buf.truncate(size);
    buf
}

/// The hex sha1 of `bytes` — the algorithm npm's `dist.shasum` names.
fn sha1_hex(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
