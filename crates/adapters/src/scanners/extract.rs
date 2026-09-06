//! Hostile-archive extraction onto disk, for the scanners that need a tree
//! (RFC 0018 §6.3 `ExtractPolicy`, §7).
//!
//! The README and SBOM readers already walk archives in memory under
//! `is_inside_root` and a per-entry byte ceiling. A scanner needs the
//! archive *on disk*, in a directory a subprocess will read, which is where
//! every classic archive attack lives — so extraction is one policy, applied
//! to every entry, that **rejects rather than truncates**:
//!
//! - a path that escapes the root, or is absolute;
//! - a symlink, a hardlink, a device, a fifo — anything that is not a
//!   regular file or a directory;
//! - more than `max_entries` entries, or more than `max_extracted_bytes` in
//!   total, or a decompression ratio above `max_ratio` (a bomb);
//! - a nested archive is written as a file and **not descended**.
//!
//! Execute bits are dropped on every file written. The scanner never runs
//! anything from the tree — that is the invariant of §7 — but a work
//! directory mounted `noexec` is belt, and this is braces.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use batlehub_core::ports::ScannerError;

/// The bounds of one extraction (RFC 0018 §6.3, `[worker.sandbox]`).
#[derive(Debug, Clone)]
pub struct ExtractPolicy {
    pub max_entries: u64,
    pub max_extracted_bytes: u64,
    /// Decompressed bytes over compressed bytes, above which the archive is a
    /// bomb whatever its declared sizes say.
    pub max_ratio: u64,
}

impl Default for ExtractPolicy {
    fn default() -> Self {
        Self {
            max_entries: 50_000,
            max_extracted_bytes: 512 * 1024 * 1024,
            max_ratio: 100,
        }
    }
}

/// What an extraction wrote.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtractReport {
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
    /// Entries left unwritten because the policy refused them by *type*
    /// (symlinks, hardlinks, devices) — counted, since a scanner may want to
    /// know the archive tried.
    pub refused_by_type: u64,
    /// Nested archives written as files and not descended.
    pub nested_archives: u64,
}

/// Why an archive was not extracted. Every one is [`ScannerError::Unsupported`]
/// or [`ScannerError::Output`] to the worker: the scan did not happen, and
/// under the default `scanner_error` that holds the artifact.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("archive entry '{0}' escapes the extraction root")]
    Traversal(String),
    #[error("archive has more than {0} entries")]
    TooManyEntries(u64),
    #[error("archive extracts to more than {0} bytes")]
    TooLarge(u64),
    #[error("archive decompression ratio exceeds {0}:1")]
    Bomb(u64),
    #[error("archive format not recognised (not a gzip tar, a zip, or a tar)")]
    Unrecognised,
    #[error("archive is malformed: {0}")]
    Malformed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<ExtractError> for ScannerError {
    fn from(e: ExtractError) -> Self {
        match e {
            ExtractError::Unrecognised => ScannerError::Unsupported(e.to_string()),
            other => ScannerError::Output(other.to_string()),
        }
    }
}

/// The archive kinds this knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Tar,
    Zip,
}

/// Sniff the format from the first bytes, never from a file name.
pub fn sniff(data: &[u8]) -> Option<ArchiveKind> {
    if data.starts_with(&[0x1f, 0x8b]) {
        return Some(ArchiveKind::TarGz);
    }
    if data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06") {
        return Some(ArchiveKind::Zip);
    }
    // A POSIX tar carries "ustar" at offset 257.
    if data.len() > 262 && &data[257..262] == b"ustar" {
        return Some(ArchiveKind::Tar);
    }
    None
}

/// Whether a file with this name is itself an archive, which is written and
/// not descended.
pub fn looks_like_nested_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".tar.gz", ".tgz", ".tar", ".zip", ".jar", ".war", ".gem", ".whl", ".nupkg", ".crate",
        ".tar.bz2", ".tar.xz", ".7z", ".rar",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// The archive-relative path, normalised, or `None` when it escapes.
fn safe_relative(raw: &str) -> Option<PathBuf> {
    let raw = raw.replace('\\', "/");
    let path = Path::new(&raw);
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    Some(out)
}

/// Running totals, checked on every entry.
struct Budget<'a> {
    policy: &'a ExtractPolicy,
    compressed: u64,
    entries: u64,
    bytes: u64,
}

impl Budget<'_> {
    fn entry(&mut self) -> Result<(), ExtractError> {
        self.entries += 1;
        if self.entries > self.policy.max_entries {
            return Err(ExtractError::TooManyEntries(self.policy.max_entries));
        }
        Ok(())
    }
    fn bytes(&mut self, n: u64) -> Result<(), ExtractError> {
        self.bytes += n;
        if self.bytes > self.policy.max_extracted_bytes {
            return Err(ExtractError::TooLarge(self.policy.max_extracted_bytes));
        }
        if self.compressed > 0 && self.bytes / self.compressed.max(1) > self.policy.max_ratio {
            return Err(ExtractError::Bomb(self.policy.max_ratio));
        }
        Ok(())
    }
}

/// Extract `data` under `root`, refusing everything the policy names.
///
/// `root` must exist and be empty or absent; it is the only directory
/// written to. Returns what was written.
pub fn extract_to(
    data: &[u8],
    root: &Path,
    policy: &ExtractPolicy,
) -> Result<ExtractReport, ExtractError> {
    let kind = sniff(data).ok_or(ExtractError::Unrecognised)?;
    std::fs::create_dir_all(root)?;
    let mut budget = Budget {
        policy,
        compressed: data.len() as u64,
        entries: 0,
        bytes: 0,
    };
    match kind {
        ArchiveKind::TarGz => {
            let decoder = flate2::read::GzDecoder::new(data);
            extract_tar(tar::Archive::new(decoder), root, &mut budget)
        }
        ArchiveKind::Tar => extract_tar(tar::Archive::new(data), root, &mut budget),
        ArchiveKind::Zip => extract_zip(data, root, &mut budget),
    }
}

fn extract_tar<R: Read>(
    mut archive: tar::Archive<R>,
    root: &Path,
    budget: &mut Budget<'_>,
) -> Result<ExtractReport, ExtractError> {
    let mut report = ExtractReport::default();
    let entries = archive
        .entries()
        .map_err(|e| ExtractError::Malformed(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| ExtractError::Malformed(e.to_string()))?;
        budget.entry()?;
        let raw = entry
            .path()
            .map_err(|e| ExtractError::Malformed(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        let Some(rel) = safe_relative(&raw) else {
            if raw.trim_end_matches('/') == "." || raw.is_empty() {
                continue;
            }
            return Err(ExtractError::Traversal(raw));
        };
        let kind = entry.header().entry_type();
        match kind {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(root.join(&rel))?;
                report.directories += 1;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => {
                let declared = entry.header().size().unwrap_or(0);
                budget.bytes(declared)?;
                let target = root.join(&rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let name = rel
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if looks_like_nested_archive(&name) {
                    report.nested_archives += 1;
                }
                let written = write_bounded(&mut entry, &target, budget, declared)?;
                // The declared size was budgeted; the actual one is what
                // counts, and a header that lied about it is caught here.
                if written > declared {
                    budget.bytes(written - declared)?;
                }
                report.files += 1;
                report.bytes += written;
            }
            // Symlinks, hardlinks, devices, fifos: refused by type.
            _ => report.refused_by_type += 1,
        }
    }
    Ok(report)
}

fn extract_zip(
    data: &[u8],
    root: &Path,
    budget: &mut Budget<'_>,
) -> Result<ExtractReport, ExtractError> {
    let mut report = ExtractReport::default();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))
        .map_err(|e| ExtractError::Malformed(e.to_string()))?;
    for i in 0..archive.len() {
        budget.entry()?;
        let mut file = archive
            .by_index(i)
            .map_err(|e| ExtractError::Malformed(e.to_string()))?;
        let raw = file.name().to_owned();
        let Some(rel) = safe_relative(&raw) else {
            return Err(ExtractError::Traversal(raw));
        };
        // A zip entry's mode carries the type in the high bits: symlinks
        // (0o120000) are refused like tar's.
        if let Some(mode) = file.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                report.refused_by_type += 1;
                continue;
            }
        }
        if file.is_dir() {
            std::fs::create_dir_all(root.join(&rel))?;
            report.directories += 1;
            continue;
        }
        budget.bytes(file.size())?;
        let target = root.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if looks_like_nested_archive(&name) {
            report.nested_archives += 1;
        }
        let declared = file.size();
        let written = write_bounded(&mut file, &target, budget, declared)?;
        if written > declared {
            budget.bytes(written - declared)?;
        }
        report.files += 1;
        report.bytes += written;
    }
    Ok(report)
}

/// Write `reader` to `target` with no execute bits, stopping the moment the
/// running total leaves the budget.
fn write_bounded<R: Read>(
    reader: &mut R,
    target: &Path,
    budget: &Budget<'_>,
    declared: u64,
) -> Result<u64, ExtractError> {
    use std::io::Write;
    // `declared` is subtracted back out because the caller already charged it
    // to `budget.bytes` before calling: without that, this entry was counted
    // twice and the ceiling was effectively halved for the largest member — an
    // archive holding one incompressible 300 MB file was refused as extracting
    // to more than 512 MiB, and `ExtractError` becomes a `ScannerError::Output`,
    // so the legitimate artifact was held.
    let remaining = budget
        .policy
        .max_extracted_bytes
        .saturating_sub(budget.bytes.saturating_sub(declared));
    let mut out = std::fs::File::create(target)?;
    let mut limited = reader.take(remaining + 1);
    let written = std::io::copy(&mut limited, &mut out)?;
    out.flush()?;
    if written > remaining {
        return Err(ExtractError::TooLarge(budget.policy.max_extracted_bytes));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *body).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    }

    /// A tar written by hand: the `tar` crate refuses to *build* an entry
    /// with `..` in its name, which is exactly the entry these tests need.
    fn raw_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, body) in entries {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..107].copy_from_slice(b"0000644");
            header[108..115].copy_from_slice(b"0000000");
            header[116..123].copy_from_slice(b"0000000");
            let size = format!("{:011o}", body.len());
            header[124..135].copy_from_slice(size.as_bytes());
            header[136..147].copy_from_slice(b"00000000000");
            header[148..156].copy_from_slice(b"        ");
            header[156] = b'0';
            header[257..262].copy_from_slice(b"ustar");
            header[263..265].copy_from_slice(b"00");
            let sum: u32 = header.iter().map(|b| *b as u32).sum();
            let chk = format!("{sum:06o}\0 ");
            header[148..156].copy_from_slice(chk.as_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(body);
            let pad = (512 - body.len() % 512) % 512;
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }

    fn zipfile(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn formats_are_sniffed_from_bytes_not_names() {
        assert_eq!(sniff(&targz(&[("a", b"x")])), Some(ArchiveKind::TarGz));
        assert_eq!(sniff(&zipfile(&[("a", b"x")])), Some(ArchiveKind::Zip));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn a_tarball_extracts_with_exec_bits_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let data = targz(&[
            ("package/package.json", b"{}"),
            ("package/bin/run", b"#!/bin/sh\n"),
        ]);
        let report = extract_to(&data, dir.path(), &ExtractPolicy::default()).unwrap();
        assert_eq!(report.files, 2);
        let run = dir.path().join("package/bin/run");
        assert!(run.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&run).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "exec bits dropped: {mode:o}");
        }
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused_not_relocated() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["../escape", "package/../../escape", "/etc/passwd"] {
            let data = raw_tar(&[(name, b"x")]);
            assert_eq!(sniff(&data), Some(ArchiveKind::Tar));
            let err = extract_to(&data, dir.path(), &ExtractPolicy::default()).unwrap_err();
            assert!(matches!(err, ExtractError::Traversal(_)), "{name}: {err}");
        }
        let data = zipfile(&[("../zipslip", b"x")]);
        let err = extract_to(&data, dir.path(), &ExtractPolicy::default()).unwrap_err();
        assert!(matches!(err, ExtractError::Traversal(_)), "{err}");
    }

    #[test]
    fn symlinks_and_hardlinks_are_refused_by_type() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_cksum();
        builder
            .append_link(&mut h, "package/link", "/etc/passwd")
            .unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        h.set_cksum();
        builder
            .append_link(&mut h, "package/hard", "package/other")
            .unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_cksum();
        builder
            .append_data(&mut h, "package/ok", &b"x"[..])
            .unwrap();
        let tar = builder.into_inner().unwrap();
        let report = extract_to(&tar, dir.path(), &ExtractPolicy::default()).unwrap();
        assert_eq!(report.refused_by_type, 2);
        assert_eq!(report.files, 1);
        assert!(!dir.path().join("package/link").exists());
    }

    #[test]
    fn the_entry_count_and_the_size_ceiling_reject_rather_than_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<(String, Vec<u8>)> = (0..12)
            .map(|i| (format!("p/f{i}"), b"x".to_vec()))
            .collect();
        let refs: Vec<(&str, &[u8])> = many
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        let data = targz(&refs);
        let policy = ExtractPolicy {
            max_entries: 10,
            ..ExtractPolicy::default()
        };
        assert!(matches!(
            extract_to(&data, dir.path(), &policy).unwrap_err(),
            ExtractError::TooManyEntries(10)
        ));

        let dir = tempfile::tempdir().unwrap();
        let big = vec![b'a'; 4096];
        let data = targz(&[("p/big", &big)]);
        let policy = ExtractPolicy {
            max_extracted_bytes: 1024,
            max_ratio: 1_000_000,
            ..ExtractPolicy::default()
        };
        assert!(matches!(
            extract_to(&data, dir.path(), &policy).unwrap_err(),
            ExtractError::TooLarge(1024)
        ));
    }

    /// A gzip of zeros: 4 MB that compresses to a few kilobytes.
    #[test]
    fn a_decompression_bomb_is_refused_by_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 4 * 1024 * 1024];
        let data = targz(&[("p/zeros", &zeros)]);
        assert!(
            data.len() < 64 * 1024,
            "the fixture compresses: {}",
            data.len()
        );
        let policy = ExtractPolicy {
            max_ratio: 100,
            ..ExtractPolicy::default()
        };
        assert!(matches!(
            extract_to(&data, dir.path(), &policy).unwrap_err(),
            ExtractError::Bomb(100)
        ));
    }

    #[test]
    fn a_nested_archive_is_written_and_not_descended() {
        let dir = tempfile::tempdir().unwrap();
        let inner = raw_tar(&[("../escape-from-inside", b"x")]);
        let data = targz(&[("p/vendored.tgz", &inner)]);
        let report = extract_to(&data, dir.path(), &ExtractPolicy::default()).unwrap();
        assert_eq!(report.nested_archives, 1);
        assert!(dir.path().join("p/vendored.tgz").is_file());
        assert!(!dir.path().join("escape-from-inside").exists());
    }

    #[test]
    fn a_zip_extracts_too() {
        let dir = tempfile::tempdir().unwrap();
        let data = zipfile(&[("lib/a.py", b"print(1)"), ("lib/sub/b.py", b"")]);
        let report = extract_to(&data, dir.path(), &ExtractPolicy::default()).unwrap();
        assert_eq!(report.files, 2);
        assert!(dir.path().join("lib/sub/b.py").is_file());
    }
}
