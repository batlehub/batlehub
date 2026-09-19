//! Reading a collection tarball at publish time (RFC 0031 §6.4, §7).
//!
//! `ansible-galaxy collection publish` posts a multipart body of `sha256` and
//! `file` and then polls the import task it is handed. The work is done before
//! that POST answers, which is what makes the task it names already finished —
//! so everything here runs synchronously on the publish request, and everything
//! it does is bounded.
//!
//! **The tarball is never extracted to a filesystem.** `MANIFEST.json` and
//! `FILES.json` are read from the archive in memory, with the `tar` crate's own
//! refusal of `..` entries, a bound on how many entries are walked and a bound
//! on how many bytes either document may be. A collection is publisher-supplied
//! and an archive is a hostile input until it has been parsed.

use std::io::Read;

use batlehub_core::error::CoreError;
use batlehub_core::services::galaxy::Manifest;
use serde_json::Value;

/// How many entries of a collection tarball are walked before the read gives
/// up.
///
/// `MANIFEST.json` and `FILES.json` are written first by every tool that builds
/// a collection, so the two documents are found in the first handful of
/// entries; the cap is what stops an archive of a million empty headers from
/// being a CPU denial in the publish handler.
const MAX_ENTRIES: usize = 4_096;

/// The largest either embedded document may be. The biggest real `FILES.json`
/// measured — `community.general`, 3 500 files — is under 2 MiB.
const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// What a publish read out of the tarball, and stores beside it.
///
/// `manifest` and `files` are stored as documents so a later read is never an
/// archive open: the version document a client resolves against is composed
/// from the publish row, and it carries both.
#[derive(Debug, Clone)]
pub struct PublishedCollection {
    pub manifest: Manifest,
    /// `MANIFEST.json` as it travelled, so the served document carries the
    /// publisher's own bytes rather than a re-serialisation of the typed
    /// subset above.
    pub manifest_json: Value,
    /// `FILES.json` as it travelled. Never re-derived: it is what a client
    /// verifies the extracted tree against, so a rebuilt copy that disagreed by
    /// one byte would fail an install this instance had accepted.
    pub files_json: Option<Value>,
}

/// Read a collection tarball's two embedded documents.
///
/// The bytes are `gzip` over `tar`; both layers are read from memory, and
/// neither is written anywhere.
pub fn read_collection_tarball(bytes: &[u8]) -> Result<PublishedCollection, CoreError> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut manifest_json: Option<Value> = None;
    let mut files_json: Option<Value> = None;

    let entries = archive
        .entries()
        .map_err(|e| CoreError::InvalidInput(format!("galaxy: not a collection tarball: {e}")))?;
    for (seen, entry) in entries.enumerate() {
        if seen >= MAX_ENTRIES {
            break;
        }
        let mut entry =
            entry.map_err(|e| CoreError::InvalidInput(format!("galaxy: tar entry: {e}")))?;
        // `tar`'s own `path()` refuses an entry that walks out of the archive,
        // which is the guard the scanner canary already relies on.
        let path = entry
            .path()
            .map_err(|e| CoreError::InvalidInput(format!("galaxy: tar entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        let slot = match path.as_str() {
            "MANIFEST.json" => &mut manifest_json,
            "FILES.json" => &mut files_json,
            _ => continue,
        };
        let mut buf = Vec::new();
        Read::take(&mut entry, MAX_DOCUMENT_BYTES)
            .read_to_end(&mut buf)
            .map_err(|e| CoreError::InvalidInput(format!("galaxy: reading {path}: {e}")))?;
        if buf.len() as u64 >= MAX_DOCUMENT_BYTES {
            return Err(CoreError::PayloadTooLarge(format!(
                "galaxy: {path} exceeds {MAX_DOCUMENT_BYTES} bytes"
            )));
        }
        *slot = Some(
            serde_json::from_slice(&buf)
                .map_err(|e| CoreError::InvalidInput(format!("galaxy: parsing {path}: {e}")))?,
        );
        if manifest_json.is_some() && files_json.is_some() {
            break;
        }
    }

    let manifest_json = manifest_json.ok_or_else(|| {
        CoreError::InvalidInput(
            "galaxy: the tarball has no MANIFEST.json at its root — it is not a collection"
                .to_owned(),
        )
    })?;
    let manifest: Manifest = serde_json::from_value(manifest_json.clone()).map_err(|e| {
        CoreError::InvalidInput(format!(
            "galaxy: MANIFEST.json is not a collection manifest: {e}"
        ))
    })?;
    Ok(PublishedCollection {
        manifest,
        manifest_json,
        files_json,
    })
}

/// The sha256 of the uploaded bytes, hex-encoded — the value the `sha256` form
/// field has to equal.
///
/// Checked before anything is stored, so a mismatched pair is a `400` rather
/// than a stored artifact nobody can install: the client hashes the body it
/// downloads and compares it with `artifact.sha256`, so bytes that do not match
/// the digest published beside them fail every install.
pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}
