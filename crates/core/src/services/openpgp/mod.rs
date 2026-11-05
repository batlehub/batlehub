//! Minimal OpenPGP (RFC 4880) **verification** of a detached signature.
//!
//! Terraform verifies a provider's `SHA256SUMS` against the namespace's keys
//! client-side, but a signature the instance stores without checking is only
//! refused at the first `terraform init`, by whoever runs it. This checks it at
//! upload, where the publisher is the one who sees the error (issue #281).
//!
//! rpgp and sequoia both pull in the `rsa` crate, banned by `deny.toml`
//! (RUSTSEC-2023-0071). That advisory is a timing leak of a *private* key; a
//! verifier holds none, and `aws-lc-rs` — already the RSA of the apk signer —
//! verifies PKCS#1 v1.5 without it. So this hand-rolls the parsing, like
//! `adapters::repo::openpgp` does for signing, and delegates the maths.
//!
//! Covered: v4 keys and v4 binary-document signatures (`gpg --detach-sign`),
//! RSA ≥ 2048 bits and Ed25519 (legacy EdDSA id 22 and RFC 9580 id 27), SHA-2.
//! Anything else is [`PgpVerdict::Unsupported`]: not refused, because Terraform
//! may still accept it, and it verifies regardless.
//! shortcut: no expiry, revocation or key-flag checks, add them if a namespace
//! ever has to refuse a key it still registers.

use aws_lc_rs::signature::{
    RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_2048_8192_SHA384,
    RSA_PKCS1_2048_8192_SHA512,
};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::error::CoreError;

const ALGO_RSA: u8 = 1;
const ALGO_RSA_SIGN: u8 = 3;
const ALGO_EDDSA_LEGACY: u8 = 22;
const ALGO_ED25519: u8 = 27;
const ED25519_OID: [u8; 9] = [0x2B, 0x06, 0x01, 0x04, 0x01, 0xDA, 0x47, 0x0F, 0x01];
const HASH_SHA256: u8 = 8;
const HASH_SHA384: u8 = 9;
const HASH_SHA512: u8 = 10;

/// What a check could establish. A signature that verifies under none of the
/// keys it names is an `Err`, not a verdict.
#[derive(Debug, PartialEq, Eq)]
pub enum PgpVerdict {
    /// Verified under the key with this long id (uppercase hex).
    Verified { key_id: String },
    /// Could not be checked here; the reason says why.
    Unsupported(String),
}

/// Verify `signature` (binary or armored) over `data` with any of the
/// armored public key blocks in `keys`.
pub fn verify_detached(
    keys: &[&str],
    signature: &[u8],
    data: &[u8],
) -> Result<PgpVerdict, CoreError> {
    let sig_bytes = dearmor_if_armored(signature)?;
    let sig = match parse_signature(&sig_bytes)? {
        Ok(sig) => sig,
        Err(reason) => return Ok(PgpVerdict::Unsupported(reason)),
    };
    let Some(issuer) = sig.issuer else {
        return Ok(PgpVerdict::Unsupported(
            "the signature names no issuer key".into(),
        ));
    };

    let mut unreadable = false;
    for armored in keys {
        let Ok(bytes) = dearmor_if_armored(armored.as_bytes()) else {
            unreadable = true;
            continue;
        };
        let Ok(found) = public_keys(&bytes) else {
            unreadable = true;
            continue;
        };
        for key in found {
            if key.key_id != issuer {
                continue;
            }
            let key_id = hex::encode_upper(key.key_id);
            return match verify_with(&key, &sig, data)? {
                Some(reason) => Ok(PgpVerdict::Unsupported(reason)),
                None => Ok(PgpVerdict::Verified { key_id }),
            };
        }
    }
    let issuer = hex::encode_upper(issuer);
    if unreadable {
        return Ok(PgpVerdict::Unsupported(format!(
            "signed by {issuer}, and a registered key could not be read"
        )));
    }
    Err(CoreError::InvalidInput(format!(
        "signed by key {issuer}, which the namespace does not register"
    )))
}

/// `Ok(None)` when it verifies, `Ok(Some(reason))` when it cannot be checked.
fn verify_with(key: &PublicKey, sig: &Sig, data: &[u8]) -> Result<Option<String>, CoreError> {
    let bad = || CoreError::InvalidInput("the signature does not verify".into());
    let hashed = || {
        let mut m = Vec::with_capacity(data.len() + sig.hashed_prefix.len() + 6);
        m.extend_from_slice(data);
        m.extend_from_slice(&sig.hashed_prefix);
        m.extend_from_slice(&[0x04, 0xFF]);
        m.extend_from_slice(&(sig.hashed_prefix.len() as u32).to_be_bytes());
        m
    };
    match (&key.material, sig.pk_algo) {
        (KeyMaterial::Rsa { n, e }, ALGO_RSA | ALGO_RSA_SIGN) => {
            if bit_len(n) < 2048 {
                return Ok(Some(format!("an RSA key of {} bits", bit_len(n))));
            }
            let params = match sig.hash_algo {
                HASH_SHA256 => &RSA_PKCS1_2048_8192_SHA256,
                HASH_SHA384 => &RSA_PKCS1_2048_8192_SHA384,
                HASH_SHA512 => &RSA_PKCS1_2048_8192_SHA512,
                h => return Ok(Some(format!("hash algorithm {h}"))),
            };
            let s = sig.mpis.first().ok_or_else(bad)?;
            // An MPI drops leading zeros; PKCS#1 wants the modulus length.
            let s = left_pad(s, n.len()).ok_or_else(bad)?;
            RsaPublicKeyComponents {
                n: n.as_slice(),
                e: e.as_slice(),
            }
            .verify(params, &hashed(), &s)
            .map_err(|_| bad())?;
            Ok(None)
        }
        (KeyMaterial::Ed25519(point), ALGO_EDDSA_LEGACY | ALGO_ED25519) => {
            let digest = match sig.hash_algo {
                HASH_SHA256 => Sha256::digest(hashed()).to_vec(),
                HASH_SHA384 => Sha384::digest(hashed()).to_vec(),
                HASH_SHA512 => Sha512::digest(hashed()).to_vec(),
                h => return Ok(Some(format!("hash algorithm {h}"))),
            };
            let raw: Vec<u8> = if sig.pk_algo == ALGO_ED25519 {
                sig.raw_tail.clone()
            } else {
                let (Some(r), Some(s)) = (sig.mpis.first(), sig.mpis.get(1)) else {
                    return Err(bad());
                };
                let mut raw = left_pad(r, 32).ok_or_else(bad)?;
                raw.extend(left_pad(s, 32).ok_or_else(bad)?);
                raw
            };
            let raw: [u8; 64] = raw.as_slice().try_into().map_err(|_| bad())?;
            let vk = VerifyingKey::from_bytes(point).map_err(|_| bad())?;
            vk.verify(&digest, &Signature::from_bytes(&raw))
                .map_err(|_| bad())?;
            Ok(None)
        }
        (KeyMaterial::Other(algo), _) => Ok(Some(format!("public-key algorithm {algo}"))),
        _ => Err(bad()),
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────

struct Sig {
    pk_algo: u8,
    hash_algo: u8,
    /// Version through the hashed subpackets: what the trailer covers.
    hashed_prefix: Vec<u8>,
    issuer: Option<[u8; 8]>,
    mpis: Vec<Vec<u8>>,
    /// The bytes after the left-16 digest: an id-27 signature is 64 raw bytes.
    raw_tail: Vec<u8>,
}

enum KeyMaterial {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ed25519([u8; 32]),
    Other(u8),
}

struct PublicKey {
    key_id: [u8; 8],
    material: KeyMaterial,
}

fn malformed(what: &str) -> CoreError {
    CoreError::InvalidInput(format!("malformed OpenPGP {what}"))
}

/// A bounds-checked reader: every input here is the caller's.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = tail;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }
    fn u16(&mut self) -> Option<usize> {
        let b = self.take(2)?;
        Some(usize::from(u16::from_be_bytes([*b.first()?, *b.get(1)?])))
    }
    fn mpi(&mut self) -> Option<Vec<u8>> {
        let bits = self.u16()?;
        Some(self.take(bits.div_ceil(8))?.to_vec())
    }
}

/// Split a packet stream into `(tag, body)`, both header formats.
fn packets(mut data: &[u8]) -> Result<Vec<(u8, &[u8])>, CoreError> {
    let err = || malformed("packet");
    let mut out = Vec::new();
    while let Some((&head, rest)) = data.split_first() {
        if head & 0x80 == 0 {
            return Err(err());
        }
        let mut c = Cursor(rest);
        let (tag, len) = if head & 0x40 != 0 {
            let first = c.u8().ok_or_else(err)?;
            let len = match first {
                0..=191 => usize::from(first),
                192..=223 => {
                    ((usize::from(first) - 192) << 8) + usize::from(c.u8().ok_or_else(err)?) + 192
                }
                255 => {
                    let b = c.take(4).ok_or_else(err)?;
                    u32::from_be_bytes(b.try_into().map_err(|_| err())?) as usize
                }
                // Partial lengths are for streamed data, never a key or a signature.
                _ => return Err(err()),
            };
            (head & 0x3F, len)
        } else {
            let len = match head & 0x03 {
                0 => usize::from(c.u8().ok_or_else(err)?),
                1 => c.u16().ok_or_else(err)?,
                2 => {
                    let b = c.take(4).ok_or_else(err)?;
                    u32::from_be_bytes(b.try_into().map_err(|_| err())?) as usize
                }
                _ => c.0.len(),
            };
            ((head >> 2) & 0x0F, len)
        };
        let body = c.take(len).ok_or_else(err)?;
        out.push((tag, body));
        data = c.0;
    }
    Ok(out)
}

/// The first signature packet; the inner `Err` is a reason it cannot be checked.
fn parse_signature(data: &[u8]) -> Result<Result<Sig, String>, CoreError> {
    let err = || malformed("signature");
    let body = packets(data)?
        .into_iter()
        .find_map(|(tag, body)| (tag == 2).then_some(body))
        .ok_or_else(err)?;
    let mut c = Cursor(body);
    let version = c.u8().ok_or_else(err)?;
    if version != 4 {
        return Ok(Err(format!("a version {version} signature")));
    }
    let sig_type = c.u8().ok_or_else(err)?;
    if sig_type != 0x00 {
        return Ok(Err(format!(
            "signature type {sig_type:#04x}, not a binary document"
        )));
    }
    let pk_algo = c.u8().ok_or_else(err)?;
    let hash_algo = c.u8().ok_or_else(err)?;
    let hashed_len = c.u16().ok_or_else(err)?;
    let hashed = c.take(hashed_len).ok_or_else(err)?;
    let hashed_prefix = body.get(..6 + hashed_len).ok_or_else(err)?.to_vec();
    let unhashed_len = c.u16().ok_or_else(err)?;
    let unhashed = c.take(unhashed_len).ok_or_else(err)?;
    let issuer = issuer_in(hashed)?.or(issuer_in(unhashed)?);
    c.take(2).ok_or_else(err)?; // left 16 bits of the digest
    let raw_tail = c.0.to_vec();
    let mut mpis = Vec::new();
    if pk_algo != ALGO_ED25519 {
        while !c.0.is_empty() {
            mpis.push(c.mpi().ok_or_else(err)?);
        }
    }
    Ok(Ok(Sig {
        pk_algo,
        hash_algo,
        hashed_prefix,
        issuer,
        mpis,
        raw_tail,
    }))
}

/// The issuer key id from subpacket 16, or a v4 fingerprint's low 64 bits (33).
fn issuer_in(mut area: &[u8]) -> Result<Option<[u8; 8]>, CoreError> {
    let err = || malformed("signature subpacket");
    let mut found = None;
    while !area.is_empty() {
        let mut c = Cursor(area);
        let first = c.u8().ok_or_else(err)?;
        let len = match first {
            0..=191 => usize::from(first),
            192..=254 => {
                ((usize::from(first) - 192) << 8) + usize::from(c.u8().ok_or_else(err)?) + 192
            }
            255 => {
                let b = c.take(4).ok_or_else(err)?;
                u32::from_be_bytes(b.try_into().map_err(|_| err())?) as usize
            }
        };
        let sp = c.take(len).ok_or_else(err)?;
        let (&kind, value) = sp.split_first().ok_or_else(err)?;
        match (kind & 0x7F, value.len()) {
            (16, 8) => found = value.try_into().ok(),
            (33, 21) if value.first() == Some(&4) => {
                found = value.get(13..).and_then(|v| v.try_into().ok());
            }
            _ => {}
        }
        area = c.0;
    }
    Ok(found)
}

/// Every v4 primary key and subkey in a key block; other versions are skipped.
fn public_keys(data: &[u8]) -> Result<Vec<PublicKey>, CoreError> {
    let err = || malformed("public key");
    let mut out = Vec::new();
    for (tag, body) in packets(data)? {
        if !matches!(tag, 6 | 14) || body.first() != Some(&4) {
            continue;
        }
        let mut h = Sha1::new();
        h.update([0x99]);
        h.update((u16::try_from(body.len()).map_err(|_| err())?).to_be_bytes());
        h.update(body);
        let fingerprint: [u8; 20] = h.finalize().into();
        let key_id: [u8; 8] = fingerprint
            .get(12..)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(err)?;

        let mut c = Cursor(body.get(5..).ok_or_else(err)?); // version + creation time
        let algo = c.u8().ok_or_else(err)?;
        let material = match algo {
            ALGO_RSA | ALGO_RSA_SIGN => KeyMaterial::Rsa {
                n: c.mpi().ok_or_else(err)?,
                e: c.mpi().ok_or_else(err)?,
            },
            ALGO_EDDSA_LEGACY => {
                let oid_len = usize::from(c.u8().ok_or_else(err)?);
                let oid = c.take(oid_len).ok_or_else(err)?;
                let point = c.mpi().ok_or_else(err)?;
                match (oid == ED25519_OID, point.split_first()) {
                    (true, Some((0x40, p))) => {
                        KeyMaterial::Ed25519(p.try_into().map_err(|_| err())?)
                    }
                    _ => KeyMaterial::Other(algo),
                }
            }
            ALGO_ED25519 => {
                KeyMaterial::Ed25519(c.take(32).ok_or_else(err)?.try_into().map_err(|_| err())?)
            }
            other => KeyMaterial::Other(other),
        };
        out.push(PublicKey { key_id, material });
    }
    Ok(out)
}

/// Binary input passes through; an ASCII-armored block is decoded (CRC ignored:
/// the signature itself is the integrity check).
fn dearmor_if_armored(data: &[u8]) -> Result<Vec<u8>, CoreError> {
    let Ok(text) = std::str::from_utf8(data) else {
        return Ok(data.to_vec());
    };
    let Some(start) = text.find("-----BEGIN PGP ") else {
        return Ok(data.to_vec());
    };
    let mut b64 = String::new();
    let mut lines = text.get(start..).unwrap_or_default().lines().skip(1);
    for line in lines.by_ref() {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if !line.contains(": ") {
            // No header block: this line is already body.
            b64.push_str(line);
            break;
        }
    }
    for line in lines {
        let line = line.trim();
        if line.starts_with('=') || line.starts_with("-----END") {
            break;
        }
        b64.push_str(line);
    }
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| malformed("armor"))
}

fn bit_len(n: &[u8]) -> usize {
    match n.iter().position(|&b| b != 0) {
        Some(i) => (n.len() - i) * 8 - n.get(i).map_or(0, |b| b.leading_zeros() as usize),
        None => 0,
    }
}

fn left_pad(v: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut out = vec![0; len.checked_sub(v.len())?];
    out.extend_from_slice(v);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made by gpg 2 (`--quick-gen-key … sign never`, `--detach-sign sums`).
    const SUMS: &[u8] = include_bytes!("testdata/sums");
    const RSA_KEY: &str = include_str!("testdata/rsa2048.asc");
    const RSA_SIG: &[u8] = include_bytes!("testdata/rsa2048.sig");
    const ED_KEY: &str = include_str!("testdata/ed25519.asc");
    const ED_SIG: &[u8] = include_bytes!("testdata/ed25519.sig");

    fn verified(id: &str) -> PgpVerdict {
        PgpVerdict::Verified { key_id: id.into() }
    }

    #[test]
    fn gpg_signatures_verify() {
        assert_eq!(
            verify_detached(&[RSA_KEY], RSA_SIG, SUMS).unwrap(),
            verified("63D5670FB64AC856")
        );
        assert_eq!(
            verify_detached(&[ED_KEY], ED_SIG, SUMS).unwrap(),
            verified("9CF09FF1369E2D26")
        );
        // The issuer picks the key out of several.
        assert_eq!(
            verify_detached(&[RSA_KEY, ED_KEY], ED_SIG, SUMS).unwrap(),
            verified("9CF09FF1369E2D26")
        );
    }

    #[test]
    fn an_armored_signature_verifies() {
        let armored = format!(
            "-----BEGIN PGP SIGNATURE-----\n\n{}\n=AAAA\n-----END PGP SIGNATURE-----\n",
            base64::engine::general_purpose::STANDARD.encode(RSA_SIG)
        );
        assert_eq!(
            verify_detached(&[RSA_KEY], armored.as_bytes(), SUMS).unwrap(),
            verified("63D5670FB64AC856")
        );
    }

    #[test]
    fn altered_data_is_refused() {
        let mut sums = SUMS.to_vec();
        sums[0] ^= 1;
        for (key, sig) in [(RSA_KEY, RSA_SIG), (ED_KEY, ED_SIG)] {
            assert!(matches!(
                verify_detached(&[key], sig, &sums),
                Err(CoreError::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn a_key_the_namespace_does_not_register_is_refused() {
        let e = verify_detached(&[ED_KEY], RSA_SIG, SUMS).unwrap_err();
        assert!(e.to_string().contains("63D5670FB64AC856"), "{e}");
    }

    #[test]
    fn garbage_is_an_error_never_a_panic() {
        for n in 0..RSA_SIG.len() {
            let _ = verify_detached(&[RSA_KEY], &RSA_SIG[..n], SUMS);
        }
        for n in 0..RSA_KEY.len() {
            let _ = verify_detached(&[&RSA_KEY[..n]], RSA_SIG, SUMS);
        }
        assert!(verify_detached(&[RSA_KEY], b"\x00junk", SUMS).is_err());
    }
}
