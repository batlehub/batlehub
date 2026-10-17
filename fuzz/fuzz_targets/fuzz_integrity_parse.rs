#![no_main]

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use libfuzzer_sys::fuzz_target;

use batlehub_core::services::integrity::{
    parse_expected, sha256_hex, verify, ChecksumAlgo, IntegrityOutcome, StreamingVerifier,
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
//     the parse, whatever chunking the stream arrives in;
//   * a checksum that *is* the body's digest verifies, in both spellings and
//     through both verifiers, and stops verifying the moment one body byte
//     changes. Random text essentially never hashes to the body, so without
//     pinning `expected` to the real digest the `Verified` arm — the one a
//     verifier that always said "mismatch" would never exercise — went unreached.
fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(fuzzed): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(chunk): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };
    // Bit 0: replace `expected` with the body's real SHA-256; bit 1: in its SRI
    // spelling rather than bare hex.
    let Ok(pin): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };
    let body: &[u8] = u.take_rest();
    let pinned = pin & 1 == 1;
    let expected = if !pinned {
        fuzzed
    } else if pin & 2 == 2 {
        let raw = hex::decode(sha256_hex(body)).expect("sha256_hex emits hex");
        format!("sha256-{}", STANDARD.encode(raw))
    } else {
        sha256_hex(body)
    };

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

    // Chunk size 0 means one chunk; anything else splits the body so the
    // hasher sees the same bytes across an arbitrary number of updates.
    let size = usize::from(chunk).max(1);
    let stream = |body: &[u8]| match StreamingVerifier::new(&expected) {
        None => IntegrityOutcome::Unparseable,
        Some(mut v) => {
            for piece in body.chunks(size) {
                v.update(piece);
            }
            v.finish()
        }
    };
    let buffered = verify(&expected, body);
    let streaming = stream(body);
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

    if pinned {
        let ok = IntegrityOutcome::Verified {
            algo: ChecksumAlgo::Sha256,
        };
        assert_eq!(
            buffered, ok,
            "the body's own digest {expected:?} did not verify"
        );
        assert_eq!(streaming, ok, "streaming refused the body's own digest");
        if !body.is_empty() {
            let idx = usize::from(chunk) % body.len();
            let mut tampered = body.to_vec();
            tampered[idx] ^= 1;
            for (how, got) in [
                ("buffered", verify(&expected, &tampered)),
                ("streaming", stream(&tampered)),
            ] {
                assert!(
                    matches!(
                        got,
                        IntegrityOutcome::Mismatch {
                            algo: ChecksumAlgo::Sha256,
                            ..
                        }
                    ),
                    "{how} verified a body with byte {idx} flipped: {got:?}"
                );
            }
        }
    }
});
