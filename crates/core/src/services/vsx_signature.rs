//! The VSIX signature archive a registry signs or relays (RFC 0020 §4.4).
//!
//! Byte for byte Open VSX's `.sigzip` — the one shape its clients read —
//! and, for the manifest, the content `vsce-sign generatemanifest` emits:
//! `package` and `entries`, each `{size, digests: {sha256}}`, entries keyed by
//! the base64 of the path inside the VSIX, digests base64. The `.signature.sig`
//! is the registry's Ed25519 signature over the **entire** VSIX file, and
//! `.signature.p7s` is empty: the editor checks that the entry exists and
//! nothing this registry could put in it would satisfy the editor's verifier
//! (§4.5), so it holds what Open VSX's holds.
//!
//! Pure functions over bytes, as core requires: reading the archive is
//! in-memory (`zip` over a cursor), and nothing here touches storage.

use std::io::{Cursor, Read, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest as _, Sha256};

use crate::error::CoreError;

/// The artifact selector the proxy fetches an upstream's signature archive
/// under — `{registry}/{ext}/{version}/vsix.sigzip` beside `…/vsix` — and
/// the suffix the local sibling key carries.
pub const SIGNATURE_ARTIFACT: &str = "vsix.sigzip";
/// The artifact selector for an upstream's public key (Open VSX serves one
/// per signing key); the Microsoft marketplace has none.
pub const PUBLIC_KEY_ARTIFACT: &str = "vsix.pubkey";

/// The three entries, in the order Open VSX writes them.
pub const ENTRY_SIG: &str = ".signature.sig";
pub const ENTRY_MANIFEST: &str = ".signature.manifest";
pub const ENTRY_P7S: &str = ".signature.p7s";

/// The signature manifest for `vsix`: the package digest and one entry per
/// file in the archive, directories skipped.
pub fn signature_manifest(vsix: &[u8]) -> Result<String, CoreError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(vsix))
        .map_err(|e| CoreError::InvalidInput(format!("VSIX is not a zip archive: {e}")))?;
    let mut entries = serde_json::Map::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| CoreError::InvalidInput(format!("VSIX entry {i} is unreadable: {e}")))?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_owned();
        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut bytes).map_err(|e| {
            CoreError::InvalidInput(format!("VSIX entry {name} is unreadable: {e}"))
        })?;
        entries.insert(STANDARD.encode(name.as_bytes()), digest_entry(&bytes));
    }
    let manifest = serde_json::json!({
        "package": digest_entry(vsix),
        "entries": entries,
    });
    serde_json::to_string(&manifest).map_err(|e| CoreError::Other(e.into()))
}

fn digest_entry(bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "size": bytes.len(),
        "digests": { "sha256": STANDARD.encode(Sha256::digest(bytes)) },
    })
}

/// The archive: `.signature.sig`, `.signature.manifest`, an empty
/// `.signature.p7s`, in that order, stored (not deflated — the entries are
/// small and Open VSX's are stored the same way).
pub fn signature_archive(manifest: &str, signature: &[u8; 64]) -> Result<Vec<u8>, CoreError> {
    let mut out = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let write = |zip: &mut zip::ZipWriter<&mut Cursor<Vec<u8>>>, name: &str, body: &[u8]| {
            zip.start_file(name, opts)
                .and_then(|()| zip.write_all(body).map_err(zip::result::ZipError::Io))
                .map_err(|e| CoreError::Other(anyhow::anyhow!("writing {name}: {e}")))
        };
        write(&mut zip, ENTRY_SIG, signature)?;
        write(&mut zip, ENTRY_MANIFEST, manifest.as_bytes())?;
        write(&mut zip, ENTRY_P7S, &[])?;
        zip.finish()
            .map_err(|e| CoreError::Other(anyhow::anyhow!("finishing the archive: {e}")))?;
    }
    Ok(out.into_inner())
}

/// What an archive holds, read back — by the client that verifies one, and
/// by the registry checking a stored sibling against its current key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureArchive {
    pub signature: Vec<u8>,
    pub manifest: String,
    pub has_p7s: bool,
}

pub fn read_signature_archive(bytes: &[u8]) -> Result<SignatureArchive, CoreError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        CoreError::InvalidInput(format!("signature archive is not a zip archive: {e}"))
    })?;
    let mut read = |name: &str| -> Result<Option<Vec<u8>>, CoreError> {
        match archive.by_name(name) {
            Ok(mut f) => {
                let mut v = Vec::new();
                f.read_to_end(&mut v).map_err(|e| {
                    CoreError::InvalidInput(format!("signature archive entry {name}: {e}"))
                })?;
                Ok(Some(v))
            }
            Err(zip::result::ZipError::FileNotFound) => Ok(None),
            Err(e) => Err(CoreError::InvalidInput(format!(
                "signature archive entry {name}: {e}"
            ))),
        }
    };
    let signature = read(ENTRY_SIG)?
        .ok_or_else(|| CoreError::InvalidInput(format!("signature archive has no {ENTRY_SIG}")))?;
    let manifest = read(ENTRY_MANIFEST)?.ok_or_else(|| {
        CoreError::InvalidInput(format!("signature archive has no {ENTRY_MANIFEST}"))
    })?;
    let has_p7s = read(ENTRY_P7S)?.is_some();
    Ok(SignatureArchive {
        signature,
        manifest: String::from_utf8(manifest)
            .map_err(|_| CoreError::InvalidInput("signature manifest is not UTF-8".into()))?,
        has_p7s,
    })
}

/// A provided archive (RFC 0020 §13.6), read for what this registry can
/// check: the manifest, and the names of the signature entries it carries —
/// the marketplace's `.signature.p7s`, Open VSX's `.signature.sig`, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidedArchive {
    pub manifest: String,
    pub entries: Vec<String>,
}

pub fn read_provided_archive(bytes: &[u8]) -> Result<ProvidedArchive, CoreError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        CoreError::InvalidInput(format!("signature archive is not a zip archive: {e}"))
    })?;
    let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    let manifest = match archive.by_name(ENTRY_MANIFEST) {
        Ok(mut f) => {
            let mut v = Vec::new();
            f.read_to_end(&mut v)
                .map_err(|e| CoreError::InvalidInput(format!("{ENTRY_MANIFEST}: {e}")))?;
            String::from_utf8(v)
                .map_err(|_| CoreError::InvalidInput("signature manifest is not UTF-8".into()))?
        }
        Err(_) => {
            return Err(CoreError::InvalidInput(format!(
                "signature archive has no {ENTRY_MANIFEST}"
            )))
        }
    };
    let entries: Vec<String> = names
        .iter()
        .filter(|n| n.as_str() == ENTRY_P7S || n.as_str() == ENTRY_SIG)
        .cloned()
        .collect();
    let signed = entries
        .iter()
        .any(|n| archive.by_name(n).map(|f| f.size() > 0).unwrap_or(false));
    if !signed {
        return Err(CoreError::InvalidInput(format!(
            "signature archive carries no signature: neither {ENTRY_P7S} nor {ENTRY_SIG} has content"
        )));
    }
    Ok(ProvidedArchive { manifest, entries })
}

/// Whether `manifest` describes `vsix`: the package digest and size, and
/// every entry's. The client-side half of a verification — the signature
/// says the bytes are the registry's, the manifest says which files are in
/// them.
pub fn manifest_matches(manifest: &str, vsix: &[u8]) -> Result<bool, CoreError> {
    let expected = signature_manifest(vsix)?;
    let a: serde_json::Value = serde_json::from_str(manifest)
        .map_err(|e| CoreError::InvalidInput(format!("signature manifest is not JSON: {e}")))?;
    let b: serde_json::Value = serde_json::from_str(&expected).expect("own output is JSON");
    Ok(a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::signature::VsxSigningKey;

    /// A two-entry VSIX-shaped zip, deterministic.
    fn vsix() -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::default());
        zip.start_file("extension.vsixmanifest", opts).unwrap();
        zip.write_all(b"<manifest/>").unwrap();
        zip.add_directory("extension/", opts).unwrap();
        zip.start_file("extension/package.json", opts).unwrap();
        zip.write_all(b"{\"name\":\"x\"}").unwrap();
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn the_manifest_has_vsce_signs_shape() {
        let bytes = vsix();
        let m: serde_json::Value =
            serde_json::from_str(&signature_manifest(&bytes).unwrap()).unwrap();
        assert_eq!(m["package"]["size"], bytes.len());
        assert_eq!(
            m["package"]["digests"]["sha256"],
            STANDARD.encode(Sha256::digest(&bytes))
        );
        let entries = m["entries"].as_object().unwrap();
        // Keyed by the base64 of the path; the directory is skipped.
        assert_eq!(entries.len(), 2, "{entries:?}");
        let key = STANDARD.encode("extension.vsixmanifest");
        assert_eq!(
            key, "ZXh0ZW5zaW9uLnZzaXhtYW5pZmVzdA==",
            "measured off vsce-sign"
        );
        assert_eq!(entries[&key]["size"], 11);
        assert_eq!(
            entries[&key]["digests"]["sha256"],
            STANDARD.encode(Sha256::digest(b"<manifest/>"))
        );
        assert!(entries.contains_key(&STANDARD.encode("extension/package.json")));
    }

    #[test]
    fn the_archive_has_open_vsxs_three_entries_and_reads_back() {
        let bytes = vsix();
        let key = VsxSigningKey::from_seed([1u8; 32], None);
        let manifest = signature_manifest(&bytes).unwrap();
        let sig = key.sign(&bytes);
        let archive = signature_archive(&manifest, &sig).unwrap();

        let mut zip = zip::ZipArchive::new(Cursor::new(&archive)).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_owned())
            .collect();
        assert_eq!(names, [ENTRY_SIG, ENTRY_MANIFEST, ENTRY_P7S]);
        assert_eq!(
            zip.by_name(ENTRY_P7S).unwrap().size(),
            0,
            "empty, as Open VSX writes it"
        );

        let back = read_signature_archive(&archive).unwrap();
        assert_eq!(back.signature, sig.to_vec());
        assert_eq!(back.manifest, manifest);
        assert!(back.has_p7s);
        assert!(key.verify(&back.signature, &bytes));
        assert!(manifest_matches(&back.manifest, &bytes).unwrap());
        assert!(!manifest_matches(&back.manifest, b"PK\x03\x04tampered").is_ok_and(|b| b));
    }

    #[test]
    fn a_provided_archive_needs_a_manifest_and_a_signature_with_content() {
        let bytes = vsix();
        let manifest = signature_manifest(&bytes).unwrap();
        // The marketplace's shape: a manifest and a p7s, no .sig.
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(ENTRY_MANIFEST, opts).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file(ENTRY_P7S, opts).unwrap();
        zip.write_all(b"\x30\x82not-really-pkcs7").unwrap();
        zip.finish().unwrap();
        let p = read_provided_archive(&out.into_inner()).unwrap();
        assert_eq!(p.entries, [ENTRY_P7S]);
        assert!(manifest_matches(&p.manifest, &bytes).unwrap());

        // Our own shape (empty p7s, a .sig) is a provided archive too.
        let ours = signature_archive(&manifest, &[7u8; 64]).unwrap();
        let p = read_provided_archive(&ours).unwrap();
        assert_eq!(p.entries, [ENTRY_SIG, ENTRY_P7S]);

        // An empty p7s and no .sig is not a signature.
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        zip.start_file(ENTRY_MANIFEST, opts).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file(ENTRY_P7S, opts).unwrap();
        zip.finish().unwrap();
        assert!(read_provided_archive(&out.into_inner()).is_err());
    }

    #[test]
    fn an_archive_without_a_signature_is_refused() {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        zip.start_file(ENTRY_MANIFEST, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        assert!(read_signature_archive(&out.into_inner()).is_err());
        assert!(read_signature_archive(b"not a zip").is_err());
        assert!(signature_manifest(b"not a zip").is_err());
    }
}
