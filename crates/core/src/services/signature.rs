//! Ed25519 detached-signature verification for downloaded artifacts.
//!
//! The signing framework stores a client-supplied detached signature
//! (`X-Artifact-Signature` + `X-Signature-Type`) per published artifact. When a
//! registry sets `signing.verify_on_download`, a stored `ed25519` signature is
//! re-checked on every download against the registry's configured
//! `trusted_keys` — verifying over the **raw artifact bytes** (the defined
//! scheme for this verifier).
//!
//! Ed25519 is the only signature algorithm verified here on purpose: RSA-based
//! crypto (the `rsa` crate, and therefore PGP/x509/Sigstore default paths) is
//! hard-banned by `deny.toml` (RUSTSEC-2023-0071). Sigstore / npm provenance
//! verification is left as a future item for that reason.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::Digest as _;

/// The signature type label that [`verify_ed25519`] handles.
pub const ED25519_SIG_TYPE: &str = "ed25519";

/// A registry's own Ed25519 signing key (RFC 0020 §4.1): the seed of
/// `[registries.vsx_signing]`, the id its public key is served under, and
/// the two forms that key is handed out in — PEM for the editor-side tools
/// that read Open VSX's `PublicKey` asset, hex for `signing.trusted_keys`.
///
/// The seed never leaves this type: `Debug` prints the key id only.
#[derive(Clone)]
pub struct VsxSigningKey {
    signing: SigningKey,
    key_id: String,
}

impl std::fmt::Debug for VsxSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VsxSigningKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl VsxSigningKey {
    /// Build from a 32-byte seed. `key_id` overrides the default id — the
    /// first 16 hex characters of SHA-256 over the raw public key, so two
    /// registries holding the same key name it the same way.
    pub fn from_seed(seed: [u8; 32], key_id: Option<&str>) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        let key_id = key_id
            .map(str::to_owned)
            .unwrap_or_else(|| default_key_id(&signing.verifying_key()));
        Self { signing, key_id }
    }

    /// Build from the hex form the configuration carries.
    pub fn from_seed_hex(seed_hex: &str, key_id: Option<&str>) -> Result<Self, String> {
        let bytes = hex::decode(seed_hex.trim()).map_err(|e| format!("seed is not hex: {e}"))?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("seed is {} bytes, an Ed25519 seed is 32", bytes.len()))?;
        Ok(Self::from_seed(seed, key_id))
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The detached signature over `data` — 64 bytes, deterministic.
    pub fn sign(&self, data: &[u8]) -> [u8; 64] {
        self.signing.sign(data).to_bytes()
    }

    /// Whether `signature` is this key's signature over `data`.
    pub fn verify(&self, signature: &[u8], data: &[u8]) -> bool {
        let Ok(sig): Result<[u8; 64], _> = signature.try_into() else {
            return false;
        };
        self.signing
            .verifying_key()
            .verify(data, &Signature::from_bytes(&sig))
            .is_ok()
    }

    /// The public key as 64 hex characters — the form `signing.trusted_keys`
    /// takes, so a registry's key can be pinned by the verifier that already
    /// exists.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    /// The public key as PEM (`SubjectPublicKeyInfo`), the form Open VSX
    /// serves at `/api/-/public-key/{id}` and its clients parse.
    pub fn public_key_pem(&self) -> String {
        public_key_pem(&self.signing.verifying_key().to_bytes())
    }
}

/// `SubjectPublicKeyInfo` for an Ed25519 key is a fixed 12-byte DER prefix
/// (`SEQUENCE { SEQUENCE { OID 1.3.101.112 }, BIT STRING }`) followed by the
/// 32 raw key bytes — small enough to write out rather than pull a DER
/// encoder in for.
pub fn public_key_pem(raw: &[u8; 32]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    const PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = Vec::with_capacity(44);
    der.extend_from_slice(&PREFIX);
    der.extend_from_slice(raw);
    let b64 = STANDARD.encode(der);
    let mut out = String::from("-----BEGIN PUBLIC KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push('\n');
    }
    out.push_str("-----END PUBLIC KEY-----\n");
    out
}

/// Read the 32 raw key bytes back out of a PEM `SubjectPublicKeyInfo` (or a
/// bare 64-hex key), for a client that fetched the registry's public key.
pub fn public_key_from_pem_or_hex(text: &str) -> Option<[u8; 32]> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let trimmed = text.trim();
    if let Ok(bytes) = hex::decode(trimmed) {
        if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Some(arr);
        }
    }
    let body: String = trimmed
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect();
    let der = STANDARD.decode(body).ok()?;
    let raw = der.strip_prefix(&[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ])?;
    <[u8; 32]>::try_from(raw).ok()
}

/// The default key id: the first 16 hex characters of SHA-256 over the raw
/// public key.
pub fn default_key_id(key: &VerifyingKey) -> String {
    let digest = sha2::Sha256::digest(key.to_bytes());
    hex::encode(&digest[..8])
}

/// Parse a hex-encoded 32-byte Ed25519 public key.
fn parse_pubkey(hex_key: &str) -> Option<VerifyingKey> {
    let bytes = hex::decode(hex_key.trim()).ok()?;
    let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

/// Verify a detached Ed25519 `signature` over `data` against any of the
/// hex-encoded `trusted_keys`.
///
/// Returns `true` only when the signature is a well-formed 64-byte Ed25519
/// signature that verifies under at least one trusted key. Malformed keys are
/// skipped; an empty key list always fails.
pub fn verify_ed25519(trusted_keys: &[String], signature: &[u8], data: &[u8]) -> bool {
    let sig: [u8; 64] = match signature.try_into() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let sig = Signature::from_bytes(&sig);
    trusted_keys
        .iter()
        .filter_map(|k| parse_pubkey(k))
        .any(|vk| vk.verify(data, &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair(seed: u8) -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pub_hex = hex::encode(sk.verifying_key().to_bytes());
        (sk, pub_hex)
    }

    #[test]
    fn the_registry_key_signs_what_the_existing_verifier_accepts() {
        let key = VsxSigningKey::from_seed([9u8; 32], None);
        let data = b"the whole vsix";
        let sig = key.sign(data);
        assert!(verify_ed25519(&[key.public_key_hex()], &sig, data));
        assert!(key.verify(&sig, data));
        assert!(!key.verify(&sig, b"other bytes"));
        assert_eq!(key.key_id().len(), 16, "sixteen hex characters by default");
        assert_eq!(
            VsxSigningKey::from_seed([9u8; 32], Some("2026-09")).key_id(),
            "2026-09"
        );
    }

    #[test]
    fn the_pem_round_trips_and_is_spki() {
        let key = VsxSigningKey::from_seed([3u8; 32], None);
        let pem = key.public_key_pem();
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA"));
        let raw = public_key_from_pem_or_hex(&pem).expect("pem parses");
        assert_eq!(hex::encode(raw), key.public_key_hex());
        assert_eq!(
            public_key_from_pem_or_hex(&key.public_key_hex()).map(hex::encode),
            Some(key.public_key_hex())
        );
        assert!(public_key_from_pem_or_hex("not a key").is_none());
    }

    #[test]
    fn a_seed_of_the_wrong_length_is_refused() {
        assert!(VsxSigningKey::from_seed_hex("abcd", None).is_err());
        assert!(VsxSigningKey::from_seed_hex(&"0".repeat(64), None).is_ok());
        assert!(VsxSigningKey::from_seed_hex("zz".repeat(32).as_str(), None).is_err());
    }

    #[test]
    fn valid_signature_with_trusted_key_verifies() {
        let (sk, pub_hex) = keypair(7);
        let data = b"artifact bytes";
        let sig = sk.sign(data).to_bytes().to_vec();
        assert!(verify_ed25519(&[pub_hex], &sig, data));
    }

    #[test]
    fn untrusted_key_fails() {
        let (sk, _) = keypair(7);
        let (_, other_pub) = keypair(9);
        let data = b"artifact bytes";
        let sig = sk.sign(data).to_bytes().to_vec();
        assert!(!verify_ed25519(&[other_pub], &sig, data));
    }

    #[test]
    fn tampered_data_fails() {
        let (sk, pub_hex) = keypair(7);
        let sig = sk.sign(b"original").to_bytes().to_vec();
        assert!(!verify_ed25519(&[pub_hex], &sig, b"tampered"));
    }

    #[test]
    fn one_trusted_key_among_many_passes() {
        let (sk, pub_hex) = keypair(3);
        let (_, other) = keypair(4);
        let data = b"abc";
        let sig = sk.sign(data).to_bytes().to_vec();
        assert!(verify_ed25519(&[other, pub_hex], &sig, data));
    }

    #[test]
    fn malformed_signature_length_fails() {
        let (_, pub_hex) = keypair(7);
        assert!(!verify_ed25519(&[pub_hex], &[1, 2, 3], b"data"));
    }

    #[test]
    fn empty_trusted_keys_fails() {
        let (sk, _) = keypair(7);
        let data = b"abc";
        let sig = sk.sign(data).to_bytes().to_vec();
        assert!(!verify_ed25519(&[], &sig, data));
    }

    #[test]
    fn malformed_trusted_key_is_skipped() {
        let (sk, pub_hex) = keypair(7);
        let data = b"abc";
        let sig = sk.sign(data).to_bytes().to_vec();
        // A bogus key entry must not break verification against a good one.
        assert!(verify_ed25519(
            &["not-hex".to_string(), pub_hex],
            &sig,
            data
        ));
    }
}
