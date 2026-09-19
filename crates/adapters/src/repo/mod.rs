//! Debian/RPM/pacman/Alpine repository hosting primitives: package parsing,
//! index generation, and signing.
//!
//! Three of the four sign with Ed25519 OpenPGP (`openpgp`); `apk` signs with
//! RSA through `aws-lc-rs`, because no shipping apk accepts anything else
//! (RFC 0026 §2.4).

pub mod openpgp;

#[cfg(feature = "registry-deb")]
pub mod deb;

#[cfg(feature = "registry-rpm")]
pub mod rpm;

#[cfg(feature = "registry-pacman")]
pub mod pacman;

#[cfg(feature = "registry-apk")]
pub mod apk;

#[cfg(feature = "registry-apk")]
pub mod apk_signer;

#[cfg(feature = "registry-apk")]
pub use apk_signer::ApkSigner;

pub use openpgp::OpenPgpSigner;

/// Gzip a byte slice (used for `Packages.gz` and the `repodata/*.xml.gz` files).
pub fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)?;
    enc.finish()
}
