//! A Nix binary cache: `nix-cache-info`, one narinfo per store path, the NARs.
//!
//! The narinfo is the interesting one to soak. It is a *document* the proxy
//! parses, blocks on, rewrites one line of, serialises and indexes — and unlike
//! every other kind's listing, one is fetched **per store path in the closure**,
//! hundreds per real system build. So if any per-document allocation here leaks,
//! this is the arm that finds it (RFC 0028 §5.1).
//!
//! The store hashes the arms request are synthetic but well-formed: 32
//! characters of Nix's own base32 alphabet, which the proxy validates at the
//! edge. A hash outside it is refused with a `400` before any work happens,
//! which would make the arm measure the validator rather than the document
//! path.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay};
use crate::Args;

/// `GET /nix/nix-cache-info`
#[route("/nix/nix-cache-info", method = "GET", method = "HEAD")]
async fn cache_info(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    HttpResponse::Ok()
        .content_type("text/x-nix-cache-info")
        .body("StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n")
}

/// `GET /nix/{hash}.narinfo` — the per-path document.
///
/// The `References:` list is five entries, matching what a real narinfo of a
/// library output carries: the fingerprint sorts and rewrites them into full
/// store paths on every relay, so a one-entry fixture would soak a code path
/// that barely runs.
#[route("/nix/{hash}.narinfo", method = "GET", method = "HEAD")]
async fn narinfo(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let hash = path.into_inner();
    // The package and version the proxy will parse out of this, by Nix's rule:
    // `demo-1.0.0` is `demo` at `1.0.0`.
    let refs: Vec<String> = (0..5)
        .map(|i| format!("{}{i}-dep{i}-1.0.0", &"0123456789abcdfghijklmnpqrsvwxyz"[..31]))
        .collect();
    let body = format!(
        "StorePath: /nix/store/{hash}-demo-1.0.0\n\
         URL: nar/{hash}.nar.zst\n\
         Compression: zstd\n\
         FileHash: sha256:10k72lz1iazridh4787xk3mfl6c5akf8x88xz7bnswc03b5gvyqp\n\
         FileSize: {size}\n\
         NarHash: sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x\n\
         NarSize: 226848\n\
         References: {refs}\n\
         Deriver: y1h1bh5gl539r42jydbnbmp3vyh11sva-demo-1.0.0.drv\n\
         Sig: cache.nixos.org-1:21qiHy652KfJ7Rsnc+dy5KndgujuIQEU/oudrFh7sWkkLlT9r8F3AxKA//dMvr9xWBA3tITPZA6ZFC7KxxRJBA==\n",
        size = args.artifact_size_kb * 1024,
        refs = refs.join(" "),
    );
    HttpResponse::Ok()
        .content_type("text/x-nix-narinfo")
        .body(body)
}

/// `GET /nix/nar/{file}` — the NAR itself, at the URL the narinfo above names.
#[route("/nix/nar/{file}", method = "GET", method = "HEAD")]
async fn nar(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let file = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/x-nix-nar")
        .body(artifact_bytes(&file, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(cache_info).service(narinfo).service(nar);
}
