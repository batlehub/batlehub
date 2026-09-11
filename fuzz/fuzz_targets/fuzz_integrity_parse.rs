#![no_main]

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use libfuzzer_sys::fuzz_target;

use batlehub_core::services::integrity::{
    parse_expected, verify, ChecksumAlgo, IntegrityOutcome, StreamingVerifier,
};

fn digest_len(algo: ChecksumAlgo) -> usize {
    match algo {
        ChecksumAlgo::Sha1 => 20,
        ChecksumAlgo::Sha256 => 32,
        ChecksumAlgo::Sha512 => 64,
    }
}

// The checksum an upstream advertises is the one thing standing between a
// cached artifact and a substituted one, and it arrives as text in three
// spellings (SRI `sha512-<base64>`, bare hex, a space-separated SRI list).
// Three properties over arbitrary text and bytes:
//
//   * a parse never yields a digest of the wrong length — a 20-byte value
//     labelled SHA-256 would compare against a truncated hash;
//   * what parses re-serialises to something that parses back to the same
//     digest, in both spellings, so a stored checksum survives a round trip;
//   * the buffered and streaming verifiers agree with each other and with
//     the parse, whatever chunking the stream arrives in.
fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(expected): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(chunk): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };
    let body: &[u8] = u.take_rest();

    let parsed = parse_expected(&expected);
    if let Some((algo, digest)) = &parsed {
        assert_eq!(
            digest.len(),
            digest_len(*algo),
            "{expected:?} parsed as {algo:?}"
        );

        let sri = format!("{}-{}", algo.as_str(), STANDARD.encode(digest));
        assert_eq!(
            parse_expected(&sri),
            parsed.clone(),
            "SRI round trip of {expected:?}"
        );
        let hex = hex::encode(digest);
        assert_eq!(
            parse_expected(&hex),
            parsed.clone(),
            "hex round trip of {expected:?}"
        );
        assert_eq!(
            parse_expected(&hex.to_ascii_uppercase()),
            parsed.clone(),
            "hex is case-insensitive"
        );
    }

    let buffered = verify(&expected, body);
    let streaming = match StreamingVerifier::new(&expected) {
        None => IntegrityOutcome::Unparseable,
        Some(mut v) => {
            // Chunk size 0 means one chunk; anything else splits the body so the
            // hasher sees the same bytes across an arbitrary number of updates.
            let size = usize::from(chunk).max(1);
            for piece in body.chunks(size) {
                v.update(piece);
            }
            v.finish()
        }
    };
    assert_eq!(
        buffered, streaming,
        "buffered and streaming disagree on {expected:?}"
    );
    assert_eq!(
        parsed.is_none(),
        matches!(buffered, IntegrityOutcome::Unparseable),
        "verify and parse_expected disagree on whether {expected:?} parses"
    );
    if let (
        Some((algo, _)),
        IntegrityOutcome::Verified { algo: got } | IntegrityOutcome::Mismatch { algo: got, .. },
    ) = (&parsed, &buffered)
    {
        assert_eq!(
            algo, got,
            "verify reports a different algorithm than the parse"
        );
    }
});
