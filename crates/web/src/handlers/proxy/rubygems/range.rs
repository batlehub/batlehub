//! Conditional and partial responses for the compact index.
//!
//! RFC 0009 §12.15. The compact index is designed to be fetched *incrementally*:
//! Bundler keeps its copy of `/versions` — tens of megabytes against a public
//! mirror — and asks for the tail with `Range: bytes=<its size>-`. This server
//! answered `200` with the whole document every time, which is a legal answer
//! (RFC 9110 §14.2 lets a server ignore `Range`) and threw away the entire point
//! of the format.
//!
//! ## What Bundler sends, and why the validator is exact
//!
//! Bundler's updater stores an etag per document and sends it back as
//! `If-None-Match` alongside the `Range` (`compact_index_client/updater.rb`,
//! `request_headers`). The etag is **opaque** to it — read from the etag file
//! and quoted, nothing more:
//!
//! ```text
//! etag = etag_path.read.tap(&:chomp!) if etag_path.file?
//! headers["If-None-Match"] = %("#{etag}") if etag
//! ```
//!
//! so the algorithm behind it is this server's choice. It issues
//! `ETag: "<sha-256 of the whole document>"`, which makes one check possible
//! that a generic file server cannot do:
//!
//! > if the client's validator equals the SHA-256 of **our document's first N
//! > bytes**, then what the client holds *is* our prefix, and appending the tail
//! > is provably correct.
//!
//! **Bundler older than 2.7 is not served by this and is not meant to be.**
//! Those versions synthesised a validator themselves when they had no stored
//! etag — `SharedHelpers.digest(:MD5).hexdigest(IO.read(path))` over the local
//! file — which is the only reason this server's etag used to be an MD5.
//! Bundler 2.7.0 removed that (2025-07-16, a breaking change) and 4.0.17 has no
//! trace of it. A pre-2.7 client now presents a validator that matches nothing
//! here, `holds_our_prefix` says no, and it gets `200` with the whole document:
//! the behaviour it had before any of this existed. Degraded by one full
//! transfer, never wrong — no client is handed a spliced document.
//!
//! With one wrinkle, which measurement found and reasoning would not have:
//! Bundler asks from one byte *before* the end of its copy — `bytes=(size-1)-` —
//! so that the answer is never empty and it never has to handle a `416`. Its
//! validator therefore describes `N+1` bytes while its range starts at `N`, and
//! a guard that only tried `N` could never match the client it exists for. Both
//! lengths are checked. The overlapping byte is one the client already has; it
//! engineered the overlap and reconciles it.
//!
//! When it does not match, the client's copy diverges somewhere inside the part
//! it is not asking for, and a `206` would hand it a document it must then
//! detect as corrupt and re-fetch. So that case answers `200` with the whole
//! document — one round trip instead of two, and still within spec.
//!
//! ## `Repr-Digest` is what makes a `206` usable at all
//!
//! Bundler will not append a partial response that carries no digest of the
//! whole document — *"appending is too error prone to do without digests"*, and
//! `CacheFile#append` returns `false` on the spot. `Updater#update` then falls
//! through to a plain re-fetch.
//!
//! So a `206` without this header buys nothing: measured, the first version of
//! this code produced exactly the sequence `GET /versions [range] -> 206` and
//! then `GET /versions -> 200`, having transferred the document one and a half
//! times to save nothing. Every full and partial answer therefore carries
//! `Repr-Digest: sha-256=:<base64>:` (RFC 9530, byte sequence per RFC 8941) over
//! the **whole** representation, which is also what the client verifies its
//! reassembled file against.
//!
//! This matters here more than it does for rubygems.org, whose `/versions` is
//! literally an append-only file. Ours is generated from a query, ordered by
//! name, so a gem published under a name that sorts early changes the middle of
//! the document. The guard is what makes serving ranges from a *generated*
//! document safe, and it is not hypothetical: it fired on the first two
//! measured runs, where a gem from an earlier run sorted after the new one and
//! turned the append into a middle-insertion. Both answered `200`, and
//! `bundle install` completed.

use actix_web::http::{header, StatusCode};
use actix_web::{HttpRequest, HttpResponse};

const COMPACT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// What the `Range` header asked for, resolved against the document's length.
#[derive(Debug, PartialEq, Eq)]
enum Resolved {
    /// No `Range`, a form we do not serve, or more than one range.
    Full,
    /// `start..=end`, inclusive, both within the document.
    Partial { start: usize, end: usize },
    /// Syntactically valid and outside the document.
    Unsatisfiable,
}

/// Resolve a `Range` header value against a document of `len` bytes.
///
/// Only `bytes` ranges, and only one of them: a multi-range request would need a
/// `multipart/byteranges` body, which no compact-index client asks for, so it is
/// answered in full rather than half-implemented.
fn resolve(range: &str, len: usize) -> Resolved {
    let Some(spec) = range.trim().strip_prefix("bytes=") else {
        return Resolved::Full;
    };
    let spec = spec.trim();
    if spec.contains(',') {
        return Resolved::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return Resolved::Full;
    };
    let (first, last) = (first.trim(), last.trim());

    // `-S`: the last S bytes.
    if first.is_empty() {
        let Ok(suffix) = last.parse::<usize>() else {
            return Resolved::Full;
        };
        if suffix == 0 || len == 0 {
            return Resolved::Unsatisfiable;
        }
        return Resolved::Partial {
            start: len.saturating_sub(suffix),
            end: len - 1,
        };
    }

    let Ok(start) = first.parse::<usize>() else {
        return Resolved::Full;
    };
    // A first-byte position at or past the end has nothing to answer with —
    // including `bytes=0-` on an empty document.
    if start >= len {
        return Resolved::Unsatisfiable;
    }

    let end = if last.is_empty() {
        len - 1
    } else {
        match last.parse::<usize>() {
            // A last-byte position past the end is clamped, not rejected.
            Ok(end) => end.min(len - 1),
            Err(_) => return Resolved::Full,
        }
    };
    if end < start {
        return Resolved::Unsatisfiable;
    }
    Resolved::Partial { start, end }
}

/// The etag over `bytes`, and the same function `holds_our_prefix` re-derives
/// over a prefix. SHA-256: the value is opaque to every client that reads it
/// back (see the module header), so there is no reason for it to be anything
/// weaker.
fn etag_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// `Repr-Digest` over the whole representation, in the one algorithm Bundler
/// supports (`SUPPORTED_DIGESTS = { "sha-256" => :SHA256 }`).
///
/// The value is an RFC 8941 byte sequence — base64 between colons — and is
/// compared against `Digest::SHA256#base64digest`, so it is padded standard
/// base64 of the raw digest, not hex.
fn repr_digest(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest as _, Sha256};
    format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(bytes)))
}

const REPR_DIGEST: &str = "Repr-Digest";

/// Strip the quoting and weakness marker an entity-tag arrives in.
fn tag_value(raw: &str) -> &str {
    raw.trim().trim_start_matches("W/").trim_matches('"')
}

/// Whether `If-None-Match` names the entity we are about to serve.
fn none_match_hits(header_value: &str, etag_hex: &str) -> bool {
    header_value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || tag_value(candidate) == etag_hex
    })
}

/// Serve a compact-index document, honouring `If-None-Match` and `Range`.
///
/// Always advertises `Accept-Ranges` and an `ETag`, because a client that is
/// never given a validator can never ask a conditional question — which is how
/// every one of these documents was fetched whole, forever.
pub(super) fn compact_response(req: &HttpRequest, body: String) -> HttpResponse {
    let bytes = body.into_bytes();
    let len = bytes.len();
    let etag_hex = etag_hex(&bytes);
    let etag = format!("\"{etag_hex}\"");

    let none_match = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());

    if let Some(value) = none_match {
        if none_match_hits(value, &etag_hex) {
            return HttpResponse::NotModified()
                .insert_header((header::ETAG, etag))
                .insert_header((header::ACCEPT_RANGES, "bytes"))
                .finish();
        }
    }

    let resolved = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map_or(Resolved::Full, |raw| resolve(raw, len));

    match resolved {
        Resolved::Unsatisfiable => HttpResponse::build(StatusCode::RANGE_NOT_SATISFIABLE)
            .insert_header((header::CONTENT_RANGE, format!("bytes */{len}")))
            .insert_header((header::ETAG, etag))
            .insert_header((header::ACCEPT_RANGES, "bytes"))
            .finish(),

        Resolved::Partial { start, end } => {
            // The prefix guard, and only for an open-ended range — the shape a
            // compact-index client sends. A closed range is somebody else's
            // request and gets ordinary HTTP.
            let open_ended = end + 1 == len;
            if open_ended && start > 0 {
                if let Some(client_tag) = none_match.map(tag_value) {
                    if !holds_our_prefix(&bytes, start, client_tag) {
                        return full(bytes, etag);
                    }
                }
            }
            HttpResponse::build(StatusCode::PARTIAL_CONTENT)
                .content_type(COMPACT_CONTENT_TYPE)
                .insert_header((header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")))
                .insert_header((header::ETAG, etag))
                .insert_header((header::ACCEPT_RANGES, "bytes"))
                .insert_header((REPR_DIGEST, repr_digest(&bytes)))
                .body(bytes[start..=end].to_vec())
        }

        Resolved::Full => full(bytes, etag),
    }
}

/// Whether `client_tag` identifies a prefix of `bytes` that reaches `start`.
///
/// Two candidate lengths: `start`, for a client that asks for exactly what it
/// lacks, and `start + 1`, for Bundler, which asks from one byte before the end
/// of its copy so the answer cannot be empty.
fn holds_our_prefix(bytes: &[u8], start: usize, client_tag: &str) -> bool {
    [start, start + 1]
        .into_iter()
        .filter(|&n| n <= bytes.len())
        .any(|n| etag_hex(&bytes[..n]) == client_tag)
}

fn full(bytes: Vec<u8>, etag: String) -> HttpResponse {
    let digest = repr_digest(&bytes);
    HttpResponse::Ok()
        .content_type(COMPACT_CONTENT_TYPE)
        .insert_header((header::ETAG, etag))
        .insert_header((header::ACCEPT_RANGES, "bytes"))
        .insert_header((REPR_DIGEST, digest))
        .body(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn an_open_ended_range_runs_to_the_end() {
        assert_eq!(
            resolve("bytes=3-", 10),
            Resolved::Partial { start: 3, end: 9 }
        );
    }

    #[test]
    fn a_closed_range_is_clamped_to_the_document() {
        assert_eq!(
            resolve("bytes=3-99", 10),
            Resolved::Partial { start: 3, end: 9 }
        );
    }

    #[test]
    fn a_suffix_range_counts_back_from_the_end() {
        assert_eq!(
            resolve("bytes=-4", 10),
            Resolved::Partial { start: 6, end: 9 }
        );
        // Longer than the document is the whole document, not an error.
        assert_eq!(
            resolve("bytes=-40", 10),
            Resolved::Partial { start: 0, end: 9 }
        );
    }

    /// The case Bundler produces when its copy is already current and the
    /// document has not grown: asking for `bytes=<len>-` has nothing to answer.
    #[test]
    fn a_start_at_or_past_the_end_is_unsatisfiable() {
        assert_eq!(resolve("bytes=10-", 10), Resolved::Unsatisfiable);
        assert_eq!(resolve("bytes=11-", 10), Resolved::Unsatisfiable);
        assert_eq!(resolve("bytes=0-", 0), Resolved::Unsatisfiable);
    }

    /// Anything we do not serve is answered whole rather than guessed at.
    #[test]
    fn unsupported_range_forms_fall_back_to_the_whole_document() {
        assert_eq!(resolve("items=1-2", 10), Resolved::Full);
        assert_eq!(resolve("bytes=0-1,5-6", 10), Resolved::Full);
        assert_eq!(resolve("bytes=abc-", 10), Resolved::Full);
        assert_eq!(resolve("bytes=1-abc", 10), Resolved::Full);
    }

    fn body() -> String {
        "---\naaa 1.0.0 x\nbbb 2.0.0 y\n".to_owned()
    }

    #[test]
    fn a_matching_validator_is_not_modified() {
        let etag = etag_hex(body().as_bytes());
        let req = TestRequest::default()
            .insert_header((header::IF_NONE_MATCH, format!("\"{etag}\"")))
            .to_http_request();
        let resp = compact_response(&req, body());
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    }

    /// The whole point: a client holding our prefix gets only what it lacks.
    #[test]
    fn a_client_holding_our_prefix_gets_the_tail() {
        let full_body = body();
        let prefix_len = 16;
        let prefix_tag = etag_hex(&full_body.as_bytes()[..prefix_len]);
        let req = TestRequest::default()
            .insert_header((header::IF_NONE_MATCH, format!("\"{prefix_tag}\"")))
            .insert_header((header::RANGE, format!("bytes={prefix_len}-")))
            .to_http_request();

        let resp = compact_response(&req, full_body.clone());
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            format!(
                "bytes {prefix_len}-{}/{}",
                full_body.len() - 1,
                full_body.len()
            )
            .as_str()
        );
    }

    /// Bundler asks from one byte before the end of what it holds, so the
    /// validator covers `start + 1` bytes. This is the request that actually
    /// arrives from `bundle install`.
    #[test]
    fn bundlers_one_byte_overlap_still_counts_as_holding_our_prefix() {
        let full_body = body();
        let held = 16; // what the client has
        let held_tag = etag_hex(&full_body.as_bytes()[..held]);
        let req = TestRequest::default()
            .insert_header((header::IF_NONE_MATCH, format!("\"{held_tag}\"")))
            .insert_header((header::RANGE, format!("bytes={}-", held - 1)))
            .to_http_request();

        let resp = compact_response(&req, full_body.clone());
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            format!(
                "bytes {}-{}/{}",
                held - 1,
                full_body.len() - 1,
                full_body.len()
            )
            .as_str()
        );
    }

    /// A client whose copy diverges inside the part it is *not* asking for gets
    /// the whole document, rather than a `206` it would have to detect as
    /// corrupt and fetch again.
    #[test]
    fn a_client_holding_something_else_gets_the_whole_document() {
        let req = TestRequest::default()
            .insert_header((
                header::IF_NONE_MATCH,
                "\"0123456789abcdef0123456789abcdef\"",
            ))
            .insert_header((header::RANGE, "bytes=16-"))
            .to_http_request();
        let resp = compact_response(&req, body());
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Without a validator there is nothing to check, and a plain `Range` gets
    /// the ordinary HTTP answer.
    #[test]
    fn a_range_without_a_validator_is_served_as_asked() {
        let req = TestRequest::default()
            .insert_header((header::RANGE, "bytes=4-"))
            .to_http_request();
        let resp = compact_response(&req, body());
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    }

    #[test]
    fn an_unsatisfiable_range_says_how_long_the_document_is() {
        let len = body().len();
        let req = TestRequest::default()
            .insert_header((header::RANGE, format!("bytes={len}-")))
            .to_http_request();
        let resp = compact_response(&req, body());
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            format!("bytes */{len}").as_str()
        );
    }

    /// Without this header Bundler discards a `206` and re-fetches, so a
    /// partial response that lacks it is worse than never having sent one.
    #[test]
    fn partial_and_full_answers_carry_a_representation_digest() {
        let expected = {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            use sha2::{Digest as _, Sha256};
            format!(
                "sha-256=:{}:",
                STANDARD.encode(Sha256::digest(body().as_bytes()))
            )
        };

        let plain = TestRequest::default().to_http_request();
        let resp = compact_response(&plain, body());
        assert_eq!(resp.headers().get(REPR_DIGEST).unwrap(), expected.as_str());

        let partial = TestRequest::default()
            .insert_header((header::RANGE, "bytes=4-"))
            .to_http_request();
        let resp = compact_response(&partial, body());
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(REPR_DIGEST).unwrap(),
            expected.as_str(),
            "the digest describes the whole document, not the slice"
        );
    }

    #[test]
    fn every_answer_advertises_that_ranges_work() {
        let req = TestRequest::default().to_http_request();
        let resp = compact_response(&req, body());
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
        assert!(resp.headers().contains_key(header::ETAG));
    }
}
