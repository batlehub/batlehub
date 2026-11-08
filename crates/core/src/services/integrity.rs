//! Checksum verification for proxied artifacts.
//!
//! Upstream registries advertise per-version digests in their metadata
//! ([`crate::entities::PackageMetadata::checksum`]). The proxy buffers the full
//! artifact before caching it (see [`crate::services::ProxyService::handle`]),
//! which is exactly the point at which the bytes can be hashed and compared
//! against that advertised digest — catching corruption or upstream tampering
//! before a bad artifact is ever written to the cache or served downstream.
//!
//! Different ecosystems encode the digest differently, so [`parse_expected`]
//! normalizes the two shapes we encounter:
//!
//! * **Subresource Integrity (SRI)** — `"<algo>-<base64>"`, e.g. npm's
//!   `integrity` field (`"sha512-…"`). The algorithm is the prefix.
//! * **Bare hex** — Cargo's sparse-index `cksum` (SHA-256), npm's legacy
//!   `shasum` (SHA-1), and PyPI's `digests.sha256` (SHA-256). The algorithm is
//!   inferred from the hex length (40 → SHA-1, 64 → SHA-256, 128 → SHA-512).

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

/// Hash algorithm used by an advertised checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgo {
    Sha1,
    Sha256,
    Sha512,
}

impl ChecksumAlgo {
    /// Stable lowercase label, used for metrics labels and log fields.
    pub fn as_str(self) -> &'static str {
        match self {
            ChecksumAlgo::Sha1 => "sha1",
            ChecksumAlgo::Sha256 => "sha256",
            ChecksumAlgo::Sha512 => "sha512",
        }
    }

    /// Digest length in bytes.
    fn digest_len(self) -> usize {
        match self {
            ChecksumAlgo::Sha1 => 20,
            ChecksumAlgo::Sha256 => 32,
            ChecksumAlgo::Sha512 => 64,
        }
    }
}

/// Outcome of verifying buffered artifact bytes against an advertised checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityOutcome {
    /// The computed digest matched the advertised one.
    Verified { algo: ChecksumAlgo },
    /// The computed digest did **not** match (corruption or tampering).
    /// `expected` and `actual` are hex-encoded for logging/finding storage.
    Mismatch {
        algo: ChecksumAlgo,
        expected: String,
        actual: String,
    },
    /// The advertised checksum could not be parsed into a known algorithm, so
    /// verification was not possible. Callers should treat this like a missing
    /// checksum (warn, do not falsely claim "verified").
    Unparseable,
}

/// Parse an advertised checksum string into `(algorithm, raw digest bytes)`.
///
/// Returns `None` when the string is empty or in an unrecognized format. SRI
/// strings may carry multiple space-separated digests; the first parseable one
/// wins.
pub fn parse_expected(s: &str) -> Option<(ChecksumAlgo, Vec<u8>)> {
    // SRI permits a space-separated list of `<algo>-<base64>` entries; the first
    // parseable token (SRI `<algo>-<base64>` or bare hex) wins.
    s.split_whitespace().find_map(parse_expected_token)
}

/// Parse a single advertised-checksum token: either SRI `<algo>-<base64>` or a
/// bare hex digest whose algorithm is inferred from its length. `None` when the
/// token is in an unrecognized format or fails to decode.
fn parse_expected_token(token: &str) -> Option<(ChecksumAlgo, Vec<u8>)> {
    if let Some((prefix, b64)) = token.split_once('-') {
        let algo = match prefix.to_ascii_lowercase().as_str() {
            "sha1" => ChecksumAlgo::Sha1,
            "sha256" => ChecksumAlgo::Sha256,
            "sha512" => ChecksumAlgo::Sha512,
            _ => return None,
        };
        let bytes = STANDARD.decode(b64).ok()?;
        return (bytes.len() == algo.digest_len()).then_some((algo, bytes));
    }

    // Bare hex: infer the algorithm from the digest length.
    let algo = match token.len() {
        40 => ChecksumAlgo::Sha1,
        64 => ChecksumAlgo::Sha256,
        128 => ChecksumAlgo::Sha512,
        _ => return None,
    };
    let bytes = hex::decode(token.to_ascii_lowercase()).ok()?;
    Some((algo, bytes))
}

/// Self-computed bare SHA-256 hex digest of `data`.
///
/// Used as the stored checksum for re-serve verification: a registry-independent
/// digest we compute ourselves when bytes are first written, then re-check on
/// every later serve. The bare-hex form is what [`verify`] infers as SHA-256 from
/// its length, so a stored value round-trips straight back through [`verify`].
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Bare SHA-1 hex digest of `data`.
///
/// Not for integrity — [`sha256_hex`] is the stored checksum and SHA-1 is not a
/// defence against anyone. This exists because Composer *defines* `dist.shasum`
/// as a SHA-1 and hashes the downloaded zip with SHA-1 to compare: publishing a
/// SHA-256 there does not degrade to "unverified", it fails every download
/// (RFC 0009 §12.16). A `sha256` dist field is an open request upstream
/// (composer#5940), not something Composer accepts today.
///
/// Composer publishing is the only caller. npm's `dist.shasum` is the same shape
/// and used to be named here, but nothing in the npm path reaches this function:
/// the proxy prefers `dist.integrity` (SSRI, usually SHA-512) and falls back to
/// the upstream's own `shasum`, and the local packument passes through whatever
/// `dist` the publishing client sent. See docs/operations/weak-hashes.md.
pub fn sha1_hex(data: &[u8]) -> String {
    use sha1::Digest as _;
    hex::encode(Sha1::digest(data))
}

/// Incremental ("streaming") counterpart to [`verify`].
///
/// Callers that want to verify a large artifact without buffering the whole
/// thing in memory build a verifier from the advertised checksum, feed it byte
/// chunks as they arrive ([`update`](Self::update)), then call
/// [`finish`](Self::finish) for the same `Verified`/`Mismatch` outcome [`verify`]
/// would have produced over the concatenated bytes. Memory use is bounded by the
/// hasher state, not the artifact size.
///
/// Constructing a verifier already consumes the "unparseable checksum" case:
/// [`new`](Self::new) returns `None` for a checksum [`verify`] would have
/// reported as [`IntegrityOutcome::Unparseable`], so [`finish`](Self::finish)
/// only ever yields `Verified` or `Mismatch`.
pub struct StreamingVerifier {
    hasher: Hasher,
    algo: ChecksumAlgo,
    expected: Vec<u8>,
}

enum Hasher {
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
}

impl StreamingVerifier {
    /// Build a verifier for the algorithm implied by `expected`, or `None` when
    /// the checksum is unparseable (the caller should treat that exactly like
    /// [`IntegrityOutcome::Unparseable`] from [`verify`]).
    pub fn new(expected: &str) -> Option<Self> {
        let (algo, expected) = parse_expected(expected)?;
        let hasher = match algo {
            ChecksumAlgo::Sha1 => Hasher::Sha1(Sha1::new()),
            ChecksumAlgo::Sha256 => Hasher::Sha256(Sha256::new()),
            ChecksumAlgo::Sha512 => Hasher::Sha512(Sha512::new()),
        };
        Some(Self {
            hasher,
            algo,
            expected,
        })
    }

    /// Feed the next chunk of artifact bytes into the running digest.
    pub fn update(&mut self, data: &[u8]) {
        match &mut self.hasher {
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    /// Finalize the digest and compare it against the advertised checksum.
    /// Always `Verified` or `Mismatch` (never `Unparseable` — see [`new`](Self::new)).
    pub fn finish(self) -> IntegrityOutcome {
        let actual = match self.hasher {
            Hasher::Sha1(h) => h.finalize().to_vec(),
            Hasher::Sha256(h) => h.finalize().to_vec(),
            Hasher::Sha512(h) => h.finalize().to_vec(),
        };
        if actual == self.expected {
            IntegrityOutcome::Verified { algo: self.algo }
        } else {
            IntegrityOutcome::Mismatch {
                algo: self.algo,
                expected: hex::encode(&self.expected),
                actual: hex::encode(&actual),
            }
        }
    }
}

/// Hash `data` and compare it against the advertised `expected` checksum.
pub fn verify(expected: &str, data: &[u8]) -> IntegrityOutcome {
    let Some((algo, expected_bytes)) = parse_expected(expected) else {
        return IntegrityOutcome::Unparseable;
    };
    let actual = match algo {
        ChecksumAlgo::Sha1 => Sha1::digest(data).to_vec(),
        ChecksumAlgo::Sha256 => Sha256::digest(data).to_vec(),
        ChecksumAlgo::Sha512 => Sha512::digest(data).to_vec(),
    };
    if actual == expected_bytes {
        IntegrityOutcome::Verified { algo }
    } else {
        IntegrityOutcome::Mismatch {
            algo,
            expected: hex::encode(&expected_bytes),
            actual: hex::encode(&actual),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SHA-256 of b"hello" = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
    const HELLO: &[u8] = b"hello";
    const HELLO_SHA256_HEX: &str =
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    const HELLO_SHA1_HEX: &str = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";

    fn hello_sri(algo: ChecksumAlgo) -> String {
        let digest = match algo {
            ChecksumAlgo::Sha1 => Sha1::digest(HELLO).to_vec(),
            ChecksumAlgo::Sha256 => Sha256::digest(HELLO).to_vec(),
            ChecksumAlgo::Sha512 => Sha512::digest(HELLO).to_vec(),
        };
        format!("{}-{}", algo.as_str(), STANDARD.encode(digest))
    }

    #[test]
    fn cargo_bare_hex_sha256_verifies() {
        assert_eq!(
            verify(HELLO_SHA256_HEX, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha256
            }
        );
    }

    #[test]
    fn pypi_uppercase_hex_is_normalized() {
        assert_eq!(
            verify(&HELLO_SHA256_HEX.to_uppercase(), HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha256
            }
        );
    }

    #[test]
    fn npm_shasum_bare_hex_sha1_verifies() {
        assert_eq!(
            verify(HELLO_SHA1_HEX, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha1
            }
        );
    }

    /// A bare-hex SHA-512 is read by its length; without it, a registry that
    /// publishes one would silently disable cache-write verification (M10).
    #[test]
    fn bare_hex_sha512_verifies_and_a_wrong_one_mismatches() {
        let hex512 = hex::encode(Sha512::digest(HELLO));
        assert_eq!(
            verify(&hex512, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha512
            }
        );
        assert!(matches!(
            verify(&hex512, b"other"),
            IntegrityOutcome::Mismatch { .. }
        ));
    }

    /// Composer compares this against its own SHA-1 of the zip (M10).
    #[test]
    fn sha1_hex_is_the_bare_sha1_digest() {
        assert_eq!(sha1_hex(HELLO), HELLO_SHA1_HEX);
    }

    #[test]
    fn npm_sri_sha512_verifies() {
        let sri = hello_sri(ChecksumAlgo::Sha512);
        assert_eq!(
            verify(&sri, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha512
            }
        );
    }

    #[test]
    fn sri_sha1_verifies() {
        let sri = hello_sri(ChecksumAlgo::Sha1);
        assert_eq!(
            verify(&sri, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha1
            }
        );
    }

    #[test]
    fn npm_sri_sha256_verifies() {
        let sri = hello_sri(ChecksumAlgo::Sha256);
        assert_eq!(
            verify(&sri, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha256
            }
        );
    }

    #[test]
    fn sri_list_first_known_algo_wins() {
        let sri = format!("{} {}", hello_sri(ChecksumAlgo::Sha512), HELLO_SHA256_HEX);
        assert_eq!(
            verify(&sri, HELLO),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha512
            }
        );
    }

    #[test]
    fn tampered_bytes_report_mismatch() {
        match verify(HELLO_SHA256_HEX, b"goodbye") {
            IntegrityOutcome::Mismatch {
                algo,
                expected,
                actual,
            } => {
                assert_eq!(algo, ChecksumAlgo::Sha256);
                assert_eq!(expected, HELLO_SHA256_HEX);
                assert_ne!(expected, actual);
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    #[test]
    fn garbage_is_unparseable() {
        assert_eq!(
            verify("not-a-checksum", HELLO),
            IntegrityOutcome::Unparseable
        );
        assert_eq!(verify("", HELLO), IntegrityOutcome::Unparseable);
        // Hex of the wrong length for any known algorithm.
        assert_eq!(verify("abcdef", HELLO), IntegrityOutcome::Unparseable);
        // Unknown SRI algorithm.
        assert_eq!(verify("md5-abcd", HELLO), IntegrityOutcome::Unparseable);
    }

    #[test]
    fn streaming_verifier_matches_verify_when_fed_in_chunks() {
        // Feed the bytes in several uneven chunks; the outcome must match the
        // one-shot `verify` over the whole input.
        let mut v = StreamingVerifier::new(HELLO_SHA256_HEX).unwrap();
        v.update(b"he");
        v.update(b"");
        v.update(b"llo");
        assert_eq!(
            v.finish(),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha256
            }
        );
    }

    #[test]
    fn streaming_verifier_reports_mismatch() {
        let mut v = StreamingVerifier::new(HELLO_SHA256_HEX).unwrap();
        v.update(b"good");
        v.update(b"bye");
        match v.finish() {
            IntegrityOutcome::Mismatch { algo, .. } => assert_eq!(algo, ChecksumAlgo::Sha256),
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    #[test]
    fn streaming_verifier_handles_sri_sha512() {
        let sri = hello_sri(ChecksumAlgo::Sha512);
        let mut v = StreamingVerifier::new(&sri).unwrap();
        v.update(HELLO);
        assert_eq!(
            v.finish(),
            IntegrityOutcome::Verified {
                algo: ChecksumAlgo::Sha512
            }
        );
    }

    #[test]
    fn streaming_verifier_rejects_unparseable_checksum() {
        assert!(StreamingVerifier::new("not-a-checksum").is_none());
        assert!(StreamingVerifier::new("").is_none());
    }

    #[test]
    fn sri_with_wrong_digest_length_is_rejected() {
        // sha512 prefix but a short (sha256-length) base64 payload.
        let bad = format!("sha512-{}", STANDARD.encode([0u8; 32]));
        assert_eq!(parse_expected(&bad), None);
    }
}
