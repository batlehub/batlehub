//! What a narinfo claims, checked against the bytes that arrived (RFC 0028 §4.4).
//!
//! # Why this exists at all
//!
//! The registry signs what it hosts, and a signature is a claim about
//! `NarHash` and `NarSize` — fields that describe the **decompressed** stream,
//! which is not what was uploaded. RFC 0028 decision 3 settles the question
//! this raises: sign what was *verified*, not what was *stated*. So the NAR is
//! decompressed once on publish, and the registry's signature covers hashes it
//! computed from bytes it holds.
//!
//! The alternative — trusting the publisher's `NarHash` and signing it — is
//! cheaper and makes the registry's signature a claim about bytes it never
//! looked at. The signature is the product here; it has to be earned.
//!
//! # What is checked, and in what order
//!
//! Cheapest first, so a wrong upload is refused before anything expensive runs:
//!
//! 1. `Compression:` names a codec this server can read at all;
//! 2. `FileSize` against the byte count (a subtraction);
//! 3. `FileHash` against SHA-256 of the compressed bytes (one pass, no allocation);
//! 4. `NarHash` / `NarSize` against the decompressed stream (one streaming pass,
//!    hashed and counted, **never buffered**).
//!
//! Every failure names the field, because the publisher's next question is
//! always "which one".

use std::io::Write;

use batlehub_core::error::CoreError;
use batlehub_core::services::nix::{self, NarFacts, NarInfo};
use sha2::{Digest, Sha256};

/// Hard cap on the decompressed size of an uploaded NAR.
///
/// The upload is attacker-controlled and the whole of it is expanded to reach
/// `NarHash`, so without a bound a small `.xz` could expand into terabytes.
/// Sized far above any realistic store path — a full toolchain output is in the
/// hundreds of MB — and deliberately independent of
/// `limits.max_artifact_size_bytes`, which bounds the *compressed* upload: the
/// two are different quantities and a compression bomb is precisely the case
/// where they diverge.
const MAX_DECOMPRESSED: u64 = 8 * 1024 * 1024 * 1024;

/// The codecs this server can verify a NAR through.
///
/// Nix defines **thirteen** (`NIX_FOR_EACH_COMPRESSION_ALGO`): `none`, `br`,
/// `bzip2`, `compress`, `grzip`, `gzip`, `lrzip`, `lz4`, `lzip`, `lzma`,
/// `lzop`, `xz`, `zstd`. Ten of them are refused here rather than accepted and
/// trusted, because accepting one this server cannot decompress would mean
/// signing a `NarHash` nobody checked — the exact trade decision 3 rejects.
///
/// `xz` is first because it is the client's **default**
/// (`Setting<CompressionAlgo> compression{this, CompressionAlgo::xz, …}`), so
/// it is the codec an ordinary `nix copy --to` actually uses.
pub const SUPPORTED_COMPRESSION: &[&str] = &["xz", "zstd", "none"];

/// A sink that hashes and counts and keeps nothing.
///
/// A NAR is large and only two numbers are wanted from it, so buffering the
/// decompressed stream to hash it afterwards would hold hundreds of megabytes
/// per concurrent publish for no reason. This is also what makes the cap
/// meaningful: the limit is on what was *produced*, and it trips before the
/// allocation it would have caused.
struct HashingSink {
    hasher: Sha256,
    len: u64,
    limit: u64,
}

impl Write for HashingSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.len += data.len() as u64;
        if self.len > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("decompressed NAR exceeds {MAX_DECOMPRESSED} bytes"),
            ));
        }
        self.hasher.update(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Check `bytes` against every claim `info` makes about them.
///
/// On success the caller may sign the fingerprint: every field the signature
/// covers has now been computed here rather than taken from the document.
pub fn check_nar(bytes: &[u8], info: &NarInfo) -> Result<NarFacts, CoreError> {
    let compression = info.get("Compression").unwrap_or("none");
    if !SUPPORTED_COMPRESSION.contains(&compression) {
        return Err(CoreError::InvalidInput(format!(
            "'Compression: {compression}' cannot be verified by this registry, so a NAR compressed \
             with it cannot be signed. Supported: {}. Set `compression` on the store URL, e.g. \
             `nix copy --to 'https://…?compression=zstd'`",
            SUPPORTED_COMPRESSION.join(", ")
        )));
    }

    // 2. FileSize — a subtraction, and the commonest truncated-upload symptom.
    let file_size = bytes.len() as u64;
    if let Some(claimed) = info.get("FileSize") {
        let claimed: u64 = claimed.parse().map_err(|_| {
            CoreError::InvalidInput(format!("'FileSize: {claimed}' is not a number"))
        })?;
        if claimed != file_size {
            return Err(CoreError::InvalidInput(format!(
                "'FileSize' says {claimed} and {file_size} bytes arrived"
            )));
        }
    }

    // 3. FileHash — over exactly the bytes this server will store and re-serve.
    let file_hash = format!("sha256:{}", nix::nix32_encode(&Sha256::digest(bytes)));
    if let Some(claimed) = info.get("FileHash") {
        if !digests_match(claimed, &file_hash)? {
            return Err(CoreError::InvalidInput(format!(
                "'FileHash' says {claimed} and the uploaded bytes hash to {file_hash}"
            )));
        }
    }

    // 4. NarHash / NarSize — over the decompressed stream, streamed.
    let mut sink = HashingSink {
        hasher: Sha256::new(),
        len: 0,
        limit: MAX_DECOMPRESSED,
    };
    decompress_into(bytes, compression, &mut sink)?;
    let nar_size = sink.len;
    let nar_hash = format!("sha256:{}", nix::nix32_encode(&sink.hasher.finalize()));

    // Required fields, so these are compared rather than defaulted: a narinfo
    // without them never reaches here (`require_fields`).
    let claimed_size: u64 = info
        .get("NarSize")
        .ok_or_else(|| CoreError::InvalidInput("corrupt NAR info file: missing 'NarSize'".into()))?
        .parse()
        .map_err(|_| CoreError::InvalidInput("'NarSize' is not a number".to_owned()))?;
    if claimed_size != nar_size {
        return Err(CoreError::InvalidInput(format!(
            "'NarSize' says {claimed_size} and the decompressed stream is {nar_size} bytes"
        )));
    }
    let claimed_hash = info.get("NarHash").ok_or_else(|| {
        CoreError::InvalidInput("corrupt NAR info file: missing 'NarHash'".into())
    })?;
    if !digests_match(claimed_hash, &nar_hash)? {
        return Err(CoreError::InvalidInput(format!(
            "'NarHash' says {claimed_hash} and the decompressed stream hashes to {nar_hash}"
        )));
    }

    Ok(NarFacts {
        file_hash,
        file_size,
        nar_hash,
        nar_size,
    })
}

/// Whether two narinfo digest fields name the same bytes.
///
/// Not a string comparison: Nix writes `sha256:{nix32}` from `to_string`, but
/// `parseHashField` accepts base16 and base64 too, and a publisher assembling a
/// narinfo by hand may use either. Comparing the spellings would refuse a
/// correct upload for writing the same digest a different way.
fn digests_match(claimed: &str, computed: &str) -> Result<bool, CoreError> {
    let raw = |field: &str| -> Option<Vec<u8>> {
        let (algo, digest) = field.split_once(':')?;
        if algo != "sha256" {
            return None;
        }
        match digest.len() {
            64 => hex::decode(digest).ok(),
            52 => nix::nix32_decode(digest, 32),
            // base64 of 32 bytes, padded.
            44 => {
                use base64::{engine::general_purpose::STANDARD, Engine as _};
                STANDARD.decode(digest).ok().filter(|b| b.len() == 32)
            }
            _ => None,
        }
    };
    let want = raw(claimed).ok_or_else(|| {
        CoreError::InvalidInput(format!(
            "'{claimed}' is not a SHA-256 digest this server can read — Nix writes \
             sha256:{{base32}}, and base16 and base64 are accepted too"
        ))
    })?;
    let got = raw(computed).expect("this server writes its own digests");
    Ok(want == got)
}

fn decompress_into(
    bytes: &[u8],
    compression: &str,
    sink: &mut HashingSink,
) -> Result<(), CoreError> {
    let bomb = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::InvalidData && e.to_string().contains("exceeds") {
            CoreError::PayloadTooLarge(e.to_string())
        } else {
            CoreError::InvalidInput(format!("the uploaded NAR is not valid {compression}: {e}"))
        }
    };
    match compression {
        "none" => sink.write_all(bytes).map_err(bomb)?,
        "zstd" => {
            let mut dec = zstd::stream::read::Decoder::new(bytes)
                .map_err(|e| CoreError::InvalidInput(format!("not valid zstd: {e}")))?;
            std::io::copy(&mut dec, sink).map(|_| ()).map_err(bomb)?;
        }
        "xz" => lzma_rs::xz_decompress(&mut std::io::Cursor::new(bytes), sink).map_err(|e| {
            // `lzma-rs` reports the cap through its own error type, so the
            // bomb case has to be recognised by message here too.
            let text = e.to_string();
            if text.contains("exceeds") {
                CoreError::PayloadTooLarge(text)
            } else {
                CoreError::InvalidInput(format!("the uploaded NAR is not valid xz: {text}"))
            }
        })?,
        other => {
            return Err(CoreError::InvalidInput(format!(
                "'Compression: {other}' is not supported"
            )))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A NAR is just bytes to this module — it never parses the format — so the
    /// fixture is arbitrary content of a realistic size.
    fn nar_bytes() -> Vec<u8> {
        b"nix-archive-1"
            .iter()
            .copied()
            .cycle()
            .take(40_000)
            .collect()
    }

    fn nix32(raw: &[u8]) -> String {
        format!("sha256:{}", nix::nix32_encode(raw))
    }

    /// Build a narinfo describing `compressed` truthfully.
    fn truthful(compressed: &[u8], plain: &[u8], compression: &str) -> NarInfo {
        let doc = format!(
            "StorePath: /nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-demo-1.0\n\
             URL: nar/x.nar\n\
             Compression: {compression}\n\
             FileHash: {}\n\
             FileSize: {}\n\
             NarHash: {}\n\
             NarSize: {}\n\
             References: \n",
            nix32(&Sha256::digest(compressed)),
            compressed.len(),
            nix32(&Sha256::digest(plain)),
            plain.len(),
        );
        NarInfo::parse(&doc).expect("the fixture parses")
    }

    #[test]
    fn an_uncompressed_nar_verifies() {
        let nar = nar_bytes();
        let info = truthful(&nar, &nar, "none");
        let v = check_nar(&nar, &info).expect("verifies");
        assert_eq!(v.nar_size, nar.len() as u64);
        assert_eq!(v.file_size, nar.len() as u64);
        assert_eq!(v.nar_hash, v.file_hash, "no compression, so the two agree");
    }

    #[test]
    fn a_zstd_nar_verifies_and_the_two_hashes_differ() {
        let nar = nar_bytes();
        let compressed = zstd::encode_all(std::io::Cursor::new(&nar), 3).unwrap();
        let info = truthful(&compressed, &nar, "zstd");
        let v = check_nar(&compressed, &info).expect("verifies");
        assert_eq!(v.nar_size, nar.len() as u64);
        assert_eq!(v.file_size, compressed.len() as u64);
        assert_ne!(
            v.nar_hash, v.file_hash,
            "FileHash covers the compressed bytes and NarHash the stream; a \
             verifier that conflated them would pass this by accident"
        );
    }

    /// The client's **default** codec, so this is the ordinary publish.
    #[test]
    fn an_xz_nar_verifies() {
        let nar = nar_bytes();
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(&nar), &mut compressed).unwrap();
        let info = truthful(&compressed, &nar, "xz");
        let v = check_nar(&compressed, &info).expect("verifies");
        assert_eq!(v.nar_size, nar.len() as u64);
    }

    /// Each field is checked, and the error says which one — because the
    /// publisher's next question is always "which".
    #[test]
    fn every_disagreeing_field_is_refused_by_name() {
        let nar = nar_bytes();
        let cases: [(&str, &str, &str); 4] = [
            ("FileSize", "999999", "FileSize"),
            (
                "FileHash",
                "sha256:0000000000000000000000000000000000000000000000000000",
                "FileHash",
            ),
            ("NarSize", "12345", "NarSize"),
            (
                "NarHash",
                "sha256:0000000000000000000000000000000000000000000000000000",
                "NarHash",
            ),
        ];
        for (field, wrong, named) in cases {
            let mut info = truthful(&nar, &nar, "none");
            info.set(field, wrong);
            let err = check_nar(&nar, &info)
                .expect_err(&format!("a wrong {field} must be refused"))
                .to_string();
            assert!(err.contains(named), "the error must name {named}: {err}");
        }
    }

    /// Ten of Nix's thirteen codecs cannot be verified here, and an unverifiable
    /// NAR must not be signed — so it is refused, and the message names what to
    /// set instead rather than leaving the publisher to guess.
    #[test]
    fn an_unverifiable_codec_is_refused_and_the_message_names_the_alternatives() {
        let nar = nar_bytes();
        for codec in [
            "bzip2", "br", "lzip", "lz4", "gzip", "lzma", "lzop", "compress",
        ] {
            let mut info = truthful(&nar, &nar, "none");
            info.set("Compression", codec);
            let err = check_nar(&nar, &info)
                .expect_err(&format!("{codec} must be refused"))
                .to_string();
            assert!(err.contains(codec), "{codec}: {err}");
            for supported in SUPPORTED_COMPRESSION {
                assert!(
                    err.contains(supported),
                    "{codec}: must name {supported}: {err}"
                );
            }
        }
    }

    /// `lzma` is a *different* algorithm from `xz` in Nix's own enum, so a
    /// verifier that matched loosely would accept a stream it cannot read.
    #[test]
    fn lzma_is_not_treated_as_xz() {
        let nar = nar_bytes();
        let mut info = truthful(&nar, &nar, "none");
        info.set("Compression", "lzma");
        assert!(check_nar(&nar, &info).is_err());
    }

    /// A publisher may spell a digest in base16 or base64 — `parseHashField`
    /// accepts both — and refusing a correct upload over its spelling would be
    /// the checksum defect of RFC 0031 §13 in the other direction.
    #[test]
    fn a_digest_in_another_base_is_still_recognised() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let nar = nar_bytes();
        let raw = Sha256::digest(&nar);
        for spelling in [
            format!("sha256:{}", hex::encode(raw)),
            format!("sha256:{}", STANDARD.encode(raw)),
        ] {
            let mut info = truthful(&nar, &nar, "none");
            info.set("NarHash", &spelling);
            info.set("FileHash", &spelling);
            check_nar(&nar, &info).unwrap_or_else(|e| panic!("{spelling} must be accepted: {e}"));
        }
    }

    /// A small upload that expands without bound must be refused as too large,
    /// not hashed to completion.
    #[test]
    fn a_decompression_bomb_is_refused_rather_than_expanded() {
        // 200 MB of zeroes compresses to a few hundred bytes and is well under
        // the real cap, so the cap is lowered for the test by checking the sink
        // directly — the cap's *mechanism* is what matters, not its value.
        let mut sink = HashingSink {
            hasher: Sha256::new(),
            len: 0,
            limit: 1024,
        };
        let big = vec![0u8; 4096];
        let err = sink.write_all(&big).expect_err("must trip the cap");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            sink.len <= 4096,
            "the sink must stop counting, not keep going"
        );
    }

    /// Bytes that are not the codec they claim to be are a `400` naming the
    /// codec, not a panic and not a silent pass.
    #[test]
    fn bytes_that_are_not_the_declared_codec_are_refused() {
        let nar = nar_bytes();
        for codec in ["zstd", "xz"] {
            let mut info = truthful(&nar, &nar, "none");
            info.set("Compression", codec);
            let err = check_nar(&nar, &info)
                .expect_err("plain bytes are not compressed")
                .to_string();
            assert!(err.contains(codec), "{codec}: {err}");
        }
    }
}
