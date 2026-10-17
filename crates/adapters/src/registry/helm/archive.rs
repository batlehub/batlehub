//! Reading a chart archive in memory (RFC 0029 §6.2).
//!
//! `helm package` writes `{name}/Chart.yaml`, `{name}/values.yaml`,
//! `{name}/templates/…` and optionally `{name}/README.md` into a gzip'd tar.
//! The archive is publisher-supplied, so it is never extracted: the `tar`
//! crate's `path()` refuses an entry that walks out of the archive, the walk
//! is bounded in entries and each document in bytes. Nested charts under
//! `{name}/charts/` have `Chart.yaml` files of their own, which is why only a
//! file exactly one directory deep is read.

use std::io::Read;

use batlehub_core::error::CoreError;
use serde_json::Value;

/// Entries walked before giving up — `Chart.yaml` is written first.
const MAX_ENTRIES: usize = 4_096;
/// The largest `Chart.yaml` read.
const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

/// The first `{root}/{file}` of the archive, with `{root}` a single segment.
fn root_file(bytes: &[u8], wanted: &[&str]) -> Result<Option<(String, Vec<u8>)>, CoreError> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let entries = archive
        .entries()
        .map_err(|e| CoreError::InvalidInput(format!("helm: not a chart archive: {e}")))?;
    for entry in entries.take(MAX_ENTRIES) {
        let mut entry =
            entry.map_err(|e| CoreError::InvalidInput(format!("helm: tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CoreError::InvalidInput(format!("helm: tar entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        let mut parts = path.split('/');
        let (Some(_root), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if !wanted.contains(&file) {
            continue;
        }
        let mut buf = Vec::new();
        Read::take(&mut entry, MAX_DOCUMENT_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|e| CoreError::InvalidInput(format!("helm: reading {path}: {e}")))?;
        if buf.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(CoreError::PayloadTooLarge(format!(
                "helm: {path} exceeds {MAX_DOCUMENT_BYTES} bytes"
            )));
        }
        return Ok(Some((file.to_owned(), buf)));
    }
    Ok(None)
}

/// `Chart.yaml`, parsed — the publish handler validates `name` and `version`
/// out of it before either reaches a storage key.
pub fn chart_yaml_from_archive(bytes: &[u8]) -> Result<Value, CoreError> {
    let (_, raw) = root_file(bytes, &["Chart.yaml"])?.ok_or_else(|| {
        CoreError::InvalidInput(
            "helm: the archive has no {chart}/Chart.yaml — it is not a packaged chart".to_owned(),
        )
    })?;
    let text = String::from_utf8(raw)
        .map_err(|_| CoreError::InvalidInput("helm: Chart.yaml is not UTF-8".to_owned()))?;
    let chart = batlehub_core::services::helm::parse_yaml(&text)
        .map_err(|e| CoreError::InvalidInput(format!("helm: Chart.yaml is not YAML: {e}")))?;
    if !chart.is_object() {
        return Err(CoreError::InvalidInput(
            "helm: Chart.yaml is not a mapping".to_owned(),
        ));
    }
    Ok(chart)
}
