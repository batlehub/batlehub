//! RSA signing for a locally hosted `apk` repository's index.
//!
//! **RSA, and without the banned crate.** RUSTSEC-2023-0071 is a timing side
//! channel in the pure-Rust `rsa` crate's private-key operations, and
//! `deny.toml` refuses that crate by name. The ban is on that implementation,
//! not on the algorithm — and there is no alternative algorithm here: apk 3.0.8
//! accepts exactly the `.SIGN.RSA512` / `.SIGN.RSA256` / `.SIGN.RSA` /
//! `.SIGN.DSA` entries 2.14 does, and `apk_verify_start` calls
//! `EVP_DigestVerifyInit` with the digest the entry names, which OpenSSL
//! refuses for an Ed25519 key. "Wait for an apk that takes Ed25519" is waiting
//! for something that did not happen.
//!
//! So the signing goes through `aws-lc-rs`: AWS-LC's constant-time
//! implementation, already this tree's TLS provider and already in the
//! dependency graph through `rustls`. `cargo deny check` staying green is the
//! regression test that this did not smuggle `rsa` back in (RFC 0026 §2.4,
//! §7, decision 2).
//!
//! **`RSA256` and nothing else.** Both shipping apk generations accept it, so
//! the SHA-1 `RSA` form Alpine's own `abuild-sign` still writes has no client
//! here to stay compatible with.

use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{KeyPair as _, RsaKeyPair, RSA_PKCS1_SHA256};

use batlehub_core::error::CoreError;

/// The smallest key this instance will sign an index with.
///
/// apk 3 warns about anything smaller and an index is a trust anchor for every
/// package a fleet installs. Rejected at boot rather than at the first publish,
/// so the operator learns from the config error and not from a client.
const MIN_KEY_BITS: usize = 2048;

/// An RSA key and the file name clients hold its public half under, plus the
/// keys it replaced.
///
/// **Rotation is additive, because apk's key lookup is.** A client installs
/// from the first `.SIGN.*` entry whose key file it holds, so an index signed
/// with the new key *and then* each retired one installs on a fleet that is
/// half-migrated: the machines that have the new key take the first signature,
/// the machines that do not fall through to one they can verify. The operator
/// drops an entry from `previous_keys` once every client has the new file
/// (RFC 0026 §11 decision 9).
pub struct ApkSigner {
    key: RsaKeyPair,
    key_name: String,
    /// Retired keys, in the order their signatures are appended — which is the
    /// order the operator listed them in, newest first.
    previous: Vec<ApkSigner>,
}

impl ApkSigner {
    /// Load a PEM private key — PKCS#8 (`BEGIN PRIVATE KEY`) or PKCS#1
    /// (`BEGIN RSA PRIVATE KEY`), which is what `openssl genrsa` writes by
    /// default.
    pub fn from_pem(pem: &str, key_name: impl Into<String>) -> Result<Self, CoreError> {
        let (label, der) = decode_pem(pem)?;
        let key = match label.as_str() {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der),
            other => {
                return Err(CoreError::Registry(format!(
                    "apk signing key: expected a PEM block labelled 'PRIVATE KEY' (PKCS#8) or \
                     'RSA PRIVATE KEY' (PKCS#1), got '{other}'"
                )))
            }
        }
        .map_err(|e| {
            // AWS-LC accepts 2048..=8192 and rejects the rest itself, with
            // `TooSmall` — a word that tells an operator nothing about what to
            // do. Translated here, because the fix ("generate a bigger key")
            // is only obvious once you know a size was the problem.
            let detail = e.to_string();
            if detail.contains("TooSmall") {
                CoreError::Registry(format!(
                    "apk signing key is too small: {MIN_KEY_BITS} bits is the minimum, because \
                     this key is the trust anchor for every package installed from this \
                     registry. Generate one with `openssl genrsa -out apk-signing.pem 4096`"
                ))
            } else {
                CoreError::Registry(format!("apk signing key is not a usable RSA key: {detail}"))
            }
        })?;

        // Belt and braces: AWS-LC's own floor is the same 2048, so this is
        // unreachable today. It is kept because the floor is *our* policy and
        // the library's is the library's — if AWS-LC ever widened its range,
        // silence here would mean a 1024-bit key signing a fleet's index.
        let bits = key.public_modulus_len() * 8;
        if bits < MIN_KEY_BITS {
            return Err(CoreError::Registry(format!(
                "apk signing key is {bits} bits; {MIN_KEY_BITS} is the minimum, because this key \
                 is the trust anchor for every package installed from this registry"
            )));
        }

        Ok(Self {
            key,
            key_name: key_name.into(),
            previous: Vec::new(),
        })
    }

    /// The file name the client holds the public key under in `/etc/apk/keys/`,
    /// and the name the signature entry carries — apk opens the key *by this
    /// name*, so the two are the same string by construction.
    pub fn key_name(&self) -> &str {
        &self.key_name
    }

    /// Attach the keys this one replaced. Order is preserved and it matters:
    /// it is the order the signatures appear in, and apk takes the first it
    /// can verify.
    pub fn with_previous(mut self, previous: Vec<ApkSigner>) -> Self {
        self.previous = previous;
        self
    }

    /// Every key this registry signs with, current first.
    pub fn all(&self) -> impl Iterator<Item = &ApkSigner> {
        std::iter::once(self).chain(self.previous.iter())
    }

    /// The key served under `keys/{name}`, current or retired.
    ///
    /// A retired key is served too: a machine that has not yet been given the
    /// new file is exactly the machine that needs to fetch the old one, and
    /// refusing it there would make rotation the thing that breaks a fleet.
    pub fn by_name(&self, name: &str) -> Option<&ApkSigner> {
        self.all().find(|s| s.key_name == name)
    }

    /// The tar entry name for a signature by this key: `.SIGN.RSA256.<key_name>`.
    pub fn signature_entry_name(&self) -> String {
        format!(".SIGN.RSA256.{}", self.key_name)
    }

    /// PKCS#1 v1.5 over SHA-256 of `bytes`.
    pub fn sign(&self, bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
        let mut signature = vec![0u8; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                bytes,
                &mut signature,
            )
            .map_err(|_| CoreError::Registry("apk index signing failed".to_owned()))?;
        Ok(signature)
    }

    /// The public key as a PEM `SubjectPublicKeyInfo` — the form
    /// `PEM_read_bio_PUBKEY` reads, which is what apk calls.
    pub fn public_key_pem(&self) -> Result<String, CoreError> {
        let der: PublicKeyX509Der =
            self.key.public_key().as_der().map_err(|_| {
                CoreError::Registry("could not encode the apk public key".to_owned())
            })?;
        Ok(encode_pem("PUBLIC KEY", der.as_ref()))
    }
}

impl std::fmt::Debug for ApkSigner {
    /// Never prints key material — this type is constructed from a secret and
    /// ends up inside app state that other things debug-print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApkSigner")
            .field("key_name", &self.key_name)
            .finish_non_exhaustive()
    }
}

/// Split a PEM block into its label and DER body.
fn decode_pem(pem: &str) -> Result<(String, Vec<u8>), CoreError> {
    let text = pem.trim();
    let begin = text
        .lines()
        .find(|l| l.starts_with("-----BEGIN "))
        .ok_or_else(|| {
            CoreError::Registry("apk signing key is not PEM: no '-----BEGIN' line".to_owned())
        })?;
    let label = begin
        .trim_start_matches("-----BEGIN ")
        .trim_end_matches('-')
        .trim()
        .to_owned();

    let body: String = text
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN "))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END "))
        .collect::<Vec<_>>()
        .join("");
    if body.is_empty() {
        return Err(CoreError::Registry(
            "apk signing key is not PEM: the block is empty".to_owned(),
        ));
    }

    let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body)
        .map_err(|e| CoreError::Registry(format!("apk signing key is not valid base64: {e}")))?;
    Ok((label, der))
}

/// Wrap DER in a PEM block, 64 characters to a line as every PEM reader expects.
fn encode_pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generated once with `openssl genrsa 2048`, in PKCS#8 (`BEGIN PRIVATE
    /// KEY`). A test key and nothing else: it signs fixtures in this file.
    const PKCS8_2048: &str = include_str!("testdata/apk_signing_2048.pkcs8.pem");
    /// The same key in PKCS#1 (`BEGIN RSA PRIVATE KEY`) — what `openssl genrsa`
    /// writes without `-outform`, so it is the form an operator is most likely
    /// to paste.
    const PKCS1_2048: &str = include_str!("testdata/apk_signing_2048.pkcs1.pem");
    /// 1024 bits: below the floor, and rejected for it.
    const PKCS8_1024: &str = include_str!("testdata/apk_signing_1024.pkcs8.pem");

    #[test]
    fn loads_a_pkcs8_key() {
        let signer = ApkSigner::from_pem(PKCS8_2048, "test@example.com-0001.rsa.pub").unwrap();
        assert_eq!(signer.key_name(), "test@example.com-0001.rsa.pub");
        assert_eq!(
            signer.signature_entry_name(),
            ".SIGN.RSA256.test@example.com-0001.rsa.pub"
        );
    }

    /// Both PEM forms must load, and both must be the *same* key — an operator
    /// who converts between them must not silently change the trust anchor.
    #[test]
    fn loads_a_pkcs1_key_as_the_same_key() {
        let a = ApkSigner::from_pem(PKCS8_2048, "k.rsa.pub").unwrap();
        let b = ApkSigner::from_pem(PKCS1_2048, "k.rsa.pub").unwrap();
        assert_eq!(a.public_key_pem().unwrap(), b.public_key_pem().unwrap());
    }

    /// A 1024-bit key is refused, and the error says *why* and what to do.
    /// AWS-LC rejects it first, with the word "TooSmall" and nothing else,
    /// which is why the message is translated rather than passed through.
    #[test]
    fn rejects_a_key_below_the_floor() {
        let err = ApkSigner::from_pem(PKCS8_1024, "k.rsa.pub")
            .unwrap_err()
            .to_string();
        assert!(err.contains("too small"), "says what is wrong: {err}");
        assert!(err.contains("2048"), "names the floor: {err}");
        assert!(err.contains("openssl genrsa"), "says what to do: {err}");
    }

    #[test]
    fn rejects_things_that_are_not_pem_keys() {
        assert!(ApkSigner::from_pem("not pem at all", "k.rsa.pub").is_err());
        assert!(ApkSigner::from_pem(
            "-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----\n",
            "k.rsa.pub"
        )
        .is_err());
        // A public key where a private one belongs.
        let public = ApkSigner::from_pem(PKCS8_2048, "k.rsa.pub")
            .unwrap()
            .public_key_pem()
            .unwrap();
        assert!(ApkSigner::from_pem(&public, "k.rsa.pub").is_err());
    }

    /// The signature must verify under the public key the key route serves —
    /// the whole contract, end to end, in the one place both halves are
    /// visible.
    #[test]
    fn signature_verifies_under_the_served_public_key() {
        use aws_lc_rs::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};

        let signer = ApkSigner::from_pem(PKCS8_2048, "k.rsa.pub").unwrap();
        let message = b"the compressed bytes of an APKINDEX data member";
        let signature = signer.sign(message).unwrap();

        let pem = signer.public_key_pem().unwrap();
        let (label, der) = decode_pem(&pem).unwrap();
        assert_eq!(label, "PUBLIC KEY");

        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &der)
            .verify(message, &signature)
            .expect("the signature verifies under the key clients are given");

        // And a different message does not.
        assert!(UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &der)
            .verify(b"different bytes", &signature)
            .is_err());
    }

    #[test]
    fn public_pem_is_wrapped_at_64_columns() {
        let pem = ApkSigner::from_pem(PKCS8_2048, "k.rsa.pub")
            .unwrap()
            .public_key_pem()
            .unwrap();
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
        assert!(pem.ends_with("-----END PUBLIC KEY-----\n"));
        for line in pem.lines().filter(|l| !l.starts_with("-----")) {
            assert!(line.len() <= 64, "line too long for a PEM reader: {line}");
        }
    }
}
