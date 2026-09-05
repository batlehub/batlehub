//! The bundle: content carried across the gap (RFC 0008 §4.3).
//!
//! ```text
//! manifest.json          # the plan, plus what was verified about each entry
//! blobs/<sha256>         # content-addressed
//! manifest.sig           # ed25519 detached signature over manifest.json
//! ```
//!
//! # Why blobs are content-addressed and not key-addressed
//!
//! Two registries holding identical bytes ship once, and the digest is the
//! identity — the same rule RFC 0004-bis §13.2's dedup uses, so a bundle is
//! compatible with it rather than a second scheme beside it. `manifest.json`
//! maps storage keys onto digests; a key is a *name for* a blob, never the
//! blob itself.
//!
//! # What the signature covers, and what it does not
//!
//! `manifest.sig` covers `manifest.json` exactly as written. It does **not**
//! cover the blobs: each blob's own name is its digest, so a blob that does
//! not hash to its name is rejected on the way in without reference to any
//! signature. That is the property that lets an import verify the manifest
//! *before* reading a single blob, and still catch a blob swapped afterwards.
//!
//! Ed25519 because it is the only signature this codebase verifies in-process
//! (the `rsa` crate is banned by `deny.toml` for RUSTSEC-2023-0071, which
//! rules out PGP and x509) and because the verification code already exists.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// The manifest format's own version, so a reader can refuse one it predates.
pub const BUNDLE_VERSION: u32 = 1;

/// The three names inside the tar. Fixed, because an importer reads the
/// manifest before it will read anything else and must not have to search.
pub const MANIFEST_PATH: &str = "manifest.json";
pub const SIGNATURE_PATH: &str = "manifest.sig";
pub const BLOB_PREFIX: &str = "blobs/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub bundle_version: u32,
    /// Who built it and when — evidence about a past check, and labelled as
    /// such everywhere it is shown (§5.2).
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan: Option<String>,
    pub entries: Vec<BundleEntry>,
}

/// One artifact: where it goes, what it is, and what the connected side
/// learned about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEntry {
    pub registry: String,
    /// The storage key this occupies on import. Untrusted input: the
    /// importer validates it before writing anything.
    pub key: String,
    /// Bare hex sha256 — the blob's name under `blobs/`.
    pub digest: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What RFC 0018 said on the connected side: `allowed`, `warned`,
    /// `quarantined`, `denied`. Recorded, never re-derived here — a
    /// disconnected instance cannot re-run cosign and must not present a
    /// recorded verdict as a live one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    /// RFC 0019: the ref this artifact answers, so the disconnected instance
    /// can serve `tarball/main` without resolving anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<BundleRef>,
    /// RFC 0008-bis §13.7: what the connected side's *documents* said about
    /// this artifact that its bytes do not — keyed by registry kind, the way
    /// the import files what it reads off the bytes. Terraform's provider
    /// download document names the publisher's signing keys and the
    /// protocols the provider speaks; neither is in the archive, the
    /// checksum list or the signature, and a download document composed
    /// without the keys leads the client to a refusal. Evidence, never an
    /// order: a key set is something the client verifies *with*, and the
    /// manifest signature covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleRef {
    pub owner_repo: String,
    /// The ref as a client spells it: `main`, `v2.60.0`.
    pub git_ref: String,
    /// `tag` or `branch`.
    pub kind: String,
    pub sha: String,
}

impl BundleManifest {
    /// The bytes the signature covers: the manifest as it will be written,
    /// serialised once and used for both.
    pub fn to_signed_bytes(&self) -> Result<Vec<u8>, CoreError> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| CoreError::Other(anyhow::anyhow!("serialising the manifest: {e}")))
    }

    /// Every distinct blob the manifest names, and how many keys point at
    /// each — the dedup this format exists for, made visible.
    pub fn blobs(&self) -> BTreeMap<&str, usize> {
        let mut out: BTreeMap<&str, usize> = BTreeMap::new();
        for e in &self.entries {
            *out.entry(e.digest.as_str()).or_default() += 1;
        }
        out
    }

    /// Refuse a manifest this build cannot read, and one whose entries are
    /// not self-consistent, before anything is written.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.bundle_version > BUNDLE_VERSION {
            return Err(CoreError::InvalidInput(format!(
                "bundle_version {} is newer than this build understands ({BUNDLE_VERSION})",
                self.bundle_version
            )));
        }
        for entry in &self.entries {
            if !is_sha256_hex(&entry.digest) {
                return Err(CoreError::InvalidInput(format!(
                    "entry '{}' has a digest that is not 64 hex characters",
                    entry.key
                )));
            }
            if entry.registry.is_empty() || entry.key.is_empty() {
                return Err(CoreError::InvalidInput(
                    "an entry names no registry or no key".to_owned(),
                ));
            }
            // The importer applies `validate_coordinate` and the storage
            // guard as well; this catches the shape here, where the error
            // can name the manifest rather than the blob store.
            if entry.key.contains("..") || entry.key.starts_with('/') {
                return Err(CoreError::InvalidInput(format!(
                    "entry key '{}' is not a storage key: a bundle names keys, and a bundle \
                     that could name any key would be a way to plant content anywhere",
                    entry.key
                )));
            }
            // Facts are filed into the `meta:` entry's `extra` under their
            // kind, so the shape is an object keyed by kind — anything else
            // would be merged as nothing and silently lose what it carried.
            if let Some(facts) = &entry.facts {
                if !facts.is_object() {
                    return Err(CoreError::InvalidInput(format!(
                        "entry '{}' carries facts that are not an object keyed by registry kind",
                        entry.key
                    )));
                }
            }
        }
        Ok(())
    }
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Verify `manifest.sig` against the trusted keys before reading a blob.
///
/// Reuses the ed25519 verification written for `signing.verify_on_download`;
/// there is one signature scheme in this codebase and this is it.
pub fn verify_manifest_signature(
    trusted_keys: &[String],
    signature: &[u8],
    manifest_bytes: &[u8],
) -> Result<(), CoreError> {
    if trusted_keys.is_empty() {
        return Err(CoreError::AccessDenied(
            "no bundle_trusted_keys are configured, so no bundle can be authenticated: an \
             air-gapped instance whose only content path is unauthenticated is worse than one \
             with no content path"
                .to_owned(),
        ));
    }
    if crate::services::signature::verify_ed25519(trusted_keys, signature, manifest_bytes) {
        Ok(())
    } else {
        Err(CoreError::AccessDenied(
            "the bundle's signature does not verify against any configured trusted key".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, digest: &str) -> BundleEntry {
        BundleEntry {
            registry: "gh".into(),
            key: key.into(),
            digest: digest.into(),
            size: 10,
            package_name: None,
            version: None,
            verdict: None,
            reason_codes: vec![],
            verified_at: None,
            git_ref: None,
            facts: None,
        }
    }

    fn manifest(entries: Vec<BundleEntry>) -> BundleManifest {
        BundleManifest {
            bundle_version: BUNDLE_VERSION,
            bundle_id: "b1".into(),
            created_at: Utc::now(),
            source_plan: Some("mise-plan.json".into()),
            entries,
        }
    }

    #[test]
    fn identical_bytes_under_two_keys_are_one_blob() {
        let digest = "a".repeat(64);
        let m = manifest(vec![
            entry("gh/one/1.0/x.tar.gz", &digest),
            entry("npm/two/2.0/x.tgz", &digest),
        ]);
        let blobs = m.blobs();
        assert_eq!(blobs.len(), 1, "the digest is the identity");
        assert_eq!(blobs[digest.as_str()], 2);
    }

    #[test]
    fn a_manifest_from_the_future_is_refused_rather_than_half_read() {
        let mut m = manifest(vec![]);
        m.bundle_version = BUNDLE_VERSION + 1;
        assert!(m.validate().is_err());
    }

    #[test]
    fn a_key_that_could_escape_the_store_is_refused_by_name() {
        for bad in ["../../etc/passwd", "/absolute/key", "gh/../../x"] {
            let m = manifest(vec![entry(bad, &"a".repeat(64))]);
            let err = m.validate().unwrap_err().to_string();
            assert!(err.contains("storage key"), "{bad}: {err}");
        }
        let m = manifest(vec![entry("gh/o/r/1.0/file.tar.gz", &"a".repeat(64))]);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn a_digest_that_is_not_a_digest_is_refused() {
        let m = manifest(vec![entry("gh/o/1.0/x", "not-a-digest")]);
        assert!(m.validate().is_err());
        assert!(is_sha256_hex(&"0123456789abcdef".repeat(4)));
        assert!(!is_sha256_hex(&"a".repeat(63)));
    }

    #[test]
    fn the_signature_is_checked_against_the_manifest_bytes() {
        use ed25519_dalek::{Signer, SigningKey};
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let public = hex::encode(signing.verifying_key().to_bytes());
        let m = manifest(vec![entry("gh/o/1.0/x", &"a".repeat(64))]);
        let bytes = m.to_signed_bytes().unwrap();
        let sig = signing.sign(&bytes).to_bytes();

        assert!(verify_manifest_signature(std::slice::from_ref(&public), &sig, &bytes).is_ok());
        // A different manifest under the same signature.
        let other = manifest(vec![entry("gh/o/2.0/x", &"b".repeat(64))]);
        assert!(verify_manifest_signature(
            std::slice::from_ref(&public),
            &sig,
            &other.to_signed_bytes().unwrap()
        )
        .is_err());
        // An untrusted signer.
        assert!(verify_manifest_signature(&["c".repeat(64)], &sig, &bytes).is_err());
        // And no keys at all is refused with the reason, not silently.
        let err = verify_manifest_signature(&[], &sig, &bytes)
            .unwrap_err()
            .to_string();
        assert!(err.contains("bundle_trusted_keys"), "{err}");
    }
}

// ── The container ────────────────────────────────────────────────────────────

/// Write a bundle: the manifest, its signature, then one blob per distinct
/// digest.
///
/// `blobs` is asked for bytes by digest and may answer `None` for a digest
/// the caller could not fetch; the entry stays in the manifest and the
/// importer rejects it for having no blob, which is a louder failure than a
/// manifest quietly rewritten to match what was available.
pub fn write_bundle<W: std::io::Write>(
    out: W,
    manifest: &BundleManifest,
    signature: &[u8],
    mut blobs: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<usize, CoreError> {
    let io = |e: std::io::Error| CoreError::Storage(format!("writing the bundle: {e}"));
    let manifest_bytes = manifest.to_signed_bytes()?;
    let mut tar = tar::Builder::new(out);

    let append = |name: &str, bytes: &[u8], tar: &mut tar::Builder<W>| -> Result<(), CoreError> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes).map_err(io)
    };
    // The manifest first, then its signature: an importer reads both before
    // it will read a blob, and a streaming reader should not have to seek.
    append(MANIFEST_PATH, &manifest_bytes, &mut tar)?;
    append(SIGNATURE_PATH, signature, &mut tar)?;

    let mut written = 0usize;
    for digest in manifest.blobs().keys() {
        let Some(bytes) = blobs(digest) else {
            continue;
        };
        append(&format!("{BLOB_PREFIX}{digest}"), &bytes, &mut tar)?;
        written += 1;
    }
    tar.into_inner().map_err(io)?;
    Ok(written)
}

/// A bundle read back: the manifest, its signature, and the blobs by digest.
#[derive(Debug)]
pub struct ReadBundle {
    pub manifest: BundleManifest,
    pub signature: Vec<u8>,
    pub blobs: BTreeMap<String, Vec<u8>>,
    /// Blobs whose bytes did not hash to their own file name. Rejected
    /// without being kept — a blob is named by its content, so a mismatch is
    /// not a blob at all.
    pub rejected: Vec<String>,
}

/// Read a bundle, checking each blob against its own name.
///
/// The signature is **not** checked here: the caller checks it against its
/// own trusted keys before writing anything, and separating the two keeps
/// this function about the container rather than about trust.
pub fn read_bundle<R: std::io::Read>(input: R) -> Result<ReadBundle, CoreError> {
    use std::io::Read;
    let io = |e: std::io::Error| CoreError::InvalidInput(format!("reading the bundle: {e}"));
    let mut archive = tar::Archive::new(input);
    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut signature: Option<Vec<u8>> = None;
    let mut blobs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut rejected: Vec<String> = Vec::new();

    for entry in archive.entries().map_err(io)? {
        let mut entry = entry.map_err(io)?;
        let path = entry
            .path()
            .map_err(io)?
            .to_string_lossy()
            .replace('\\', "/");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(io)?;
        if path == MANIFEST_PATH {
            manifest_bytes = Some(bytes);
        } else if path == SIGNATURE_PATH {
            signature = Some(bytes);
        } else if let Some(name) = path.strip_prefix(BLOB_PREFIX) {
            // The name is the digest, and nothing else is read from the
            // path: a blob called `../../etc/passwd` is not a digest and is
            // rejected here, before any key is derived from it.
            if !is_sha256_hex(name) {
                rejected.push(path.clone());
                continue;
            }
            if crate::services::integrity::sha256_hex(&bytes) == name {
                blobs.insert(name.to_owned(), bytes);
            } else {
                rejected.push(name.to_owned());
            }
        }
        // Anything else in the tar is ignored rather than refused: a future
        // version may add a file this build does not know, and the manifest
        // and signature are what this build reads.
    }

    let manifest_bytes = manifest_bytes
        .ok_or_else(|| CoreError::InvalidInput("the bundle has no manifest.json".to_owned()))?;
    let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| CoreError::InvalidInput(format!("manifest.json is not a manifest: {e}")))?;
    manifest.validate()?;
    Ok(ReadBundle {
        manifest,
        signature: signature
            .ok_or_else(|| CoreError::InvalidInput("the bundle has no manifest.sig".to_owned()))?,
        blobs,
        rejected,
    })
}

/// The manifest bytes exactly as they were written, for the signature check.
///
/// Re-serialising the parsed manifest would work only while serialisation is
/// byte-stable, which is not a promise `serde_json` makes across versions —
/// so the check reads the file rather than the value.
pub fn manifest_bytes_of<R: std::io::Read>(input: R) -> Result<Vec<u8>, CoreError> {
    use std::io::Read;
    let io = |e: std::io::Error| CoreError::InvalidInput(format!("reading the bundle: {e}"));
    let mut archive = tar::Archive::new(input);
    for entry in archive.entries().map_err(io)? {
        let mut entry = entry.map_err(io)?;
        let path = entry.path().map_err(io)?.to_string_lossy().into_owned();
        if path == MANIFEST_PATH {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(io)?;
            return Ok(bytes);
        }
    }
    Err(CoreError::InvalidInput(
        "the bundle has no manifest.json".to_owned(),
    ))
}

#[cfg(test)]
mod container_tests {
    use super::*;

    fn manifest() -> BundleManifest {
        BundleManifest {
            bundle_version: BUNDLE_VERSION,
            bundle_id: "b1".into(),
            created_at: Utc::now(),
            source_plan: None,
            entries: vec![
                BundleEntry {
                    registry: "gh".into(),
                    key: "gh/cli/cli/2.60.0/gh.tar.gz".into(),
                    digest: crate::services::integrity::sha256_hex(b"one"),
                    size: 3,
                    package_name: Some("cli/cli".into()),
                    version: Some("2.60.0".into()),
                    verdict: Some("allowed".into()),
                    reason_codes: vec![],
                    verified_at: Some(Utc::now()),
                    git_ref: None,
                    facts: None,
                },
                // The same bytes under a second key: one blob.
                BundleEntry {
                    registry: "npm".into(),
                    key: "npm/left-pad/1.3.1/left-pad.tgz".into(),
                    digest: crate::services::integrity::sha256_hex(b"one"),
                    size: 3,
                    package_name: None,
                    version: None,
                    verdict: None,
                    reason_codes: vec![],
                    verified_at: None,
                    git_ref: None,
                    facts: None,
                },
            ],
        }
    }

    #[test]
    fn a_bundle_round_trips_and_ships_shared_bytes_once() {
        let m = manifest();
        let mut out = Vec::new();
        let written = write_bundle(&mut out, &m, b"signature-bytes", |d| {
            (d == crate::services::integrity::sha256_hex(b"one")).then(|| b"one".to_vec())
        })
        .unwrap();
        assert_eq!(written, 1, "two keys, one blob");

        let read = read_bundle(&out[..]).unwrap();
        assert_eq!(read.manifest.entries.len(), 2);
        assert_eq!(read.signature, b"signature-bytes");
        assert_eq!(read.blobs.len(), 1);
        assert!(read.rejected.is_empty());
        assert_eq!(read.manifest.bundle_id, "b1");
        // And the bytes the signature covers come back verbatim.
        assert_eq!(
            manifest_bytes_of(&out[..]).unwrap(),
            m.to_signed_bytes().unwrap()
        );
    }

    #[test]
    fn a_blob_that_does_not_hash_to_its_name_is_rejected_not_written() {
        let m = manifest();
        let mut out = Vec::new();
        // The writer is handed the wrong bytes for the digest.
        write_bundle(&mut out, &m, b"sig", |_| Some(b"tampered".to_vec())).unwrap();
        let read = read_bundle(&out[..]).unwrap();
        assert!(read.blobs.is_empty(), "a mismatched blob is not a blob");
        assert_eq!(read.rejected.len(), 1);
    }

    #[test]
    fn a_bundle_with_no_manifest_or_no_signature_is_refused() {
        let mut out = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut out);
            let bytes = b"nothing";
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_cksum();
            tar.append_data(&mut h, "readme.txt", &bytes[..]).unwrap();
            tar.finish().unwrap();
        }
        let err = read_bundle(&out[..]).unwrap_err().to_string();
        assert!(err.contains("manifest.json"), "{err}");
    }

    /// A blob whose *name* is a path rather than a digest cannot become a
    /// storage key: it is rejected while it is still a name.
    #[test]
    fn a_blob_named_like_a_path_never_becomes_a_key() {
        let m = manifest();
        let mut out = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut out);
            let manifest_bytes = m.to_signed_bytes().unwrap();
            for (name, bytes) in [
                (MANIFEST_PATH, manifest_bytes.as_slice()),
                (SIGNATURE_PATH, b"sig".as_slice()),
                ("blobs/etc-passwd", b"root:x:0:0".as_slice()),
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_size(bytes.len() as u64);
                h.set_cksum();
                tar.append_data(&mut h, name, bytes).unwrap();
            }
            tar.finish().unwrap();
        }
        let read = read_bundle(&out[..]).unwrap();
        assert!(read.blobs.is_empty());
        assert_eq!(read.rejected, vec!["blobs/etc-passwd"]);
    }
}
