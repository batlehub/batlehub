#![no_main]
//! `scanners::extract::extract_to` — the archive reader that unpacks a
//! published package on disk before a scanner runs on it (RFC 0018 §6.3).
//!
//! It reads bytes a publisher chose, into a directory the worker owns, under
//! a budget. Three things must hold for any input, and each has a name in
//! the history of archive readers:
//!
//! 1. **Nothing lands outside the root.** Not through `..`, not through an
//!    absolute path, not through a symlink or hard link pointing out — after
//!    extraction, every entry under the root resolves inside it, and no
//!    file was written anywhere else (a canary directory beside the root
//!    stays empty).
//! 2. **The budget is the budget.** Bytes written never exceed the policy's
//!    `max_extracted_bytes`, the entry count never exceeds `max_entries`,
//!    and a report that says N bytes matches what is on disk.
//! 3. **No panic.** Raw bytes, or an archive built here from fuzzed entry
//!    names, sizes and kinds — a directory, a file, a symlink, a hard link,
//!    a name with `..`, a name with a NUL — are an `Err`, or a report.
//!
//! Half the inputs are raw bytes (the three sniffed formats' headers are
//! rarely hit by chance, but a truncated or corrupt member is), half are tar,
//! tar.gz and zip archives assembled from fuzzed entries, which is what
//! reaches the interesting branches.
//!
//! A tar is built one of two ways, chosen per input. The tidy way goes through
//! `Header::set_path` and `append_data`, which *refuse* a `..` component or an
//! absolute path — so every hostile name in `name()` used to be dropped before
//! the reader ever saw it, and property 1 held vacuously for tar. The raw way
//! writes the name and link-name bytes straight into the header fields and
//! appends with `append`, which is what a hostile publisher's own tool does.

use std::io::Write;
use std::path::{Path, PathBuf};

use libfuzzer_sys::fuzz_target;

use batlehub_adapters::scanners::extract::{extract_to, ExtractPolicy};

/// One entry of an archive the fuzzer describes.
#[derive(Debug)]
enum Entry {
    File { name: String, data: Vec<u8> },
    Dir { name: String },
    Symlink { name: String, target: String },
    HardLink { name: String, target: String },
}

fn name(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    Ok(match u.int_in_range(0..=7u8)? {
        0 => u.arbitrary::<String>()?,
        1 => "../escape".to_owned(),
        2 => "/abs/olute".to_owned(),
        3 => "a/./b/../c".to_owned(),
        4 => "nested/dir/file.txt".to_owned(),
        5 => "with\\backslash\\..\\up".to_owned(),
        6 => format!("deep/{}", "x/".repeat(u.int_in_range(0..=40)?)),
        _ => format!("f{}", u.int_in_range(0..=9u8)?),
    })
}

fn entries(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for _ in 0..u.int_in_range(0..=12u8)? {
        out.push(match u.int_in_range(0..=3u8)? {
            0 => Entry::Dir { name: name(u)? },
            1 => Entry::Symlink {
                name: name(u)?,
                target: name(u)?,
            },
            2 => Entry::HardLink {
                name: name(u)?,
                target: name(u)?,
            },
            _ => {
                let len = u.int_in_range(0..=4096usize)?;
                Entry::File {
                    name: name(u)?,
                    data: u.bytes(len)?.to_vec(),
                }
            }
        });
    }
    Ok(out)
}

/// Append one entry with `name` and `link` written raw into the old-style
/// header fields, so nothing cleans or refuses them on the way in. Bytes past
/// a field's 100 are dropped, and a NUL ends the name where the reader will
/// end it.
fn append_raw(
    builder: &mut tar::Builder<Vec<u8>>,
    kind: tar::EntryType,
    name: &str,
    link: &str,
    data: &[u8],
) {
    let mut h = tar::Header::new_gnu();
    let old = h.as_old_mut();
    let n = name.len().min(old.name.len());
    old.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    let l = link.len().min(old.linkname.len());
    old.linkname[..l].copy_from_slice(&link.as_bytes()[..l]);
    h.set_entry_type(kind);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_size(data.len() as u64);
    h.set_cksum();
    let _ = builder.append(&h, data);
}

fn tar_bytes(entries: &[Entry], gz: bool, raw: bool) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for e in entries {
        if raw {
            let (kind, name, link, data): (_, &str, &str, &[u8]) = match e {
                Entry::File { name, data } => (tar::EntryType::Regular, name, "", data),
                Entry::Dir { name } => (tar::EntryType::Directory, name, "", &[]),
                Entry::Symlink { name, target } => (tar::EntryType::Symlink, name, target, &[]),
                Entry::HardLink { name, target } => (tar::EntryType::Link, name, target, &[]),
            };
            append_raw(&mut builder, kind, name, link, data);
            continue;
        }
        let mut h = tar::Header::new_gnu();
        h.set_mode(0o644);
        h.set_mtime(0);
        // A name tar itself refuses (too long, NUL) is a fixture the reader
        // must never see; skip it rather than fail the builder.
        let ok = match e {
            Entry::File { name, data } => {
                h.set_size(data.len() as u64);
                h.set_entry_type(tar::EntryType::Regular);
                h.set_path(name).is_ok() && {
                    h.set_cksum();
                    builder.append_data(&mut h, name, data.as_slice()).is_ok()
                }
            }
            Entry::Dir { name } => {
                h.set_size(0);
                h.set_entry_type(tar::EntryType::Directory);
                h.set_path(name).is_ok() && {
                    h.set_cksum();
                    builder.append_data(&mut h, name, std::io::empty()).is_ok()
                }
            }
            Entry::Symlink { name, target } => {
                h.set_size(0);
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_path(name).is_ok() && h.set_link_name(target).is_ok() && {
                    h.set_cksum();
                    builder.append_data(&mut h, name, std::io::empty()).is_ok()
                }
            }
            Entry::HardLink { name, target } => {
                h.set_size(0);
                h.set_entry_type(tar::EntryType::Link);
                h.set_path(name).is_ok() && h.set_link_name(target).is_ok() && {
                    h.set_cksum();
                    builder.append_data(&mut h, name, std::io::empty()).is_ok()
                }
            }
        };
        let _ = ok;
    }
    let raw = builder.into_inner().unwrap_or_default();
    if gz {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let _ = enc.write_all(&raw);
        enc.finish().unwrap_or_default()
    } else {
        raw
    }
}

fn zip_bytes(entries: &[Entry]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for e in entries {
        match e {
            Entry::File { name, data } => {
                if w.start_file(name.as_str(), opts).is_ok() {
                    let _ = w.write_all(data);
                }
            }
            Entry::Dir { name } => {
                let _ = w.add_directory(name.as_str(), opts);
            }
            Entry::Symlink { name, target } | Entry::HardLink { name, target } => {
                let _ = w.add_symlink(name.as_str(), target.as_str(), opts);
            }
        }
    }
    w.finish().map(|c| c.into_inner()).unwrap_or_default()
}

/// The regular members of a tar or tar.gz, as normalised paths — `None` when
/// the bytes are not a tar this can list.
///
/// Normalised the way any extractor must (`\\` as `/`, `.` dropped), so two
/// members that land on one file read as one path. A later member then
/// *overwrites* the earlier: the report has counted both writes and the disk
/// holds one file. That is what the fuzzer found first once raw names reached
/// the reader — two `deep/x/x/…` names that became identical when truncated to
/// the header's 100 bytes — and it is a property of tar, not a leak: the
/// budget charged both writes, so the report stays an upper bound.
fn regular_members(archive: &[u8]) -> Option<Vec<PathBuf>> {
    let raw = if archive.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(archive), &mut out).ok()?;
        out
    } else {
        archive.to_vec()
    };
    let mut a = tar::Archive::new(&raw[..]);
    let mut out = Vec::new();
    for e in a.entries().ok()? {
        let e = e.ok()?;
        use tar::EntryType::{Continuous, GNUSparse, Regular};
        if matches!(e.header().entry_type(), Regular | Continuous | GNUSparse) {
            let p = String::from_utf8_lossy(&e.path_bytes()).replace('\\', "/");
            out.push(
                Path::new(&p)
                    .components()
                    .filter(|c| matches!(c, std::path::Component::Normal(_)))
                    .collect(),
            );
        }
    }
    Some(out)
}

/// Every path under `root` resolves inside `root`; nothing under `canary`.
fn check_tree(root: &Path, canary: &Path) -> (u64, u64) {
    let real_root = std::fs::canonicalize(root).expect("the root exists");
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path: PathBuf = entry.path();
            let meta = std::fs::symlink_metadata(&path).expect("readable");
            if meta.file_type().is_symlink() {
                // A link is allowed to exist; what it points at must not be
                // read through to the outside. `canonicalize` follows it.
                if let Ok(target) = std::fs::canonicalize(&path) {
                    assert!(
                        target.starts_with(&real_root),
                        "symlink {} escapes the root to {}",
                        path.display(),
                        target.display()
                    );
                }
                continue;
            }
            let real = std::fs::canonicalize(&path).expect("resolvable");
            assert!(
                real.starts_with(&real_root),
                "{} is outside the root",
                real.display()
            );
            if meta.is_dir() {
                stack.push(path);
            } else {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    let leaked = std::fs::read_dir(canary).map(Iterator::count).unwrap_or(0);
    assert_eq!(leaked, 0, "an entry was written beside the root");
    (files, bytes)
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(shape) = u.int_in_range(0..=3u8) else {
        return;
    };
    let Ok(max_entries) = u.int_in_range(1..=64u64) else {
        return;
    };
    let Ok(max_bytes) = u.int_in_range(1..=1 << 20u64) else {
        return;
    };
    let Ok(max_ratio) = u.int_in_range(1..=1000u64) else {
        return;
    };
    let policy = ExtractPolicy {
        max_entries,
        max_extracted_bytes: max_bytes,
        max_ratio,
    };

    let Ok(raw) = u.arbitrary::<bool>() else {
        return;
    };
    let archive: Vec<u8> = match shape {
        0 => u.bytes(u.len()).map(<[u8]>::to_vec).unwrap_or_default(),
        1 => tar_bytes(&entries(&mut u).unwrap_or_default(), false, raw),
        2 => tar_bytes(&entries(&mut u).unwrap_or_default(), true, raw),
        _ => zip_bytes(&entries(&mut u).unwrap_or_default()),
    };

    let work = tempfile::tempdir().expect("a temp dir");
    let root = work.path().join("root");
    let canary = work.path().join("canary");
    std::fs::create_dir_all(&canary).expect("canary");

    match extract_to(&archive, &root, &policy) {
        Ok(report) => {
            let (files, bytes) = check_tree(&root, &canary);
            assert!(
                report.bytes <= policy.max_extracted_bytes,
                "report says {} bytes over a budget of {}",
                report.bytes,
                policy.max_extracted_bytes
            );
            assert!(
                report.files + report.directories <= policy.max_entries,
                "{} entries over a budget of {}",
                report.files + report.directories,
                policy.max_entries
            );
            // The report is an upper bound on the disk always, and exact
            // whenever no two tar members land on the same file (see
            // `regular_members` for why an overwrite is not a finding).
            assert!(
                files <= report.files && bytes <= report.bytes,
                "the disk holds more than the report says: {files} files / {bytes} bytes \
                 against {report:?}"
            );
            if let Some(members) = regular_members(&archive) {
                let distinct: std::collections::HashSet<_> = members.iter().collect();
                if distinct.len() == members.len() {
                    assert_eq!(
                        report.files, files,
                        "the report's file count is not what is on disk ({members:?})"
                    );
                    assert_eq!(
                        report.bytes, bytes,
                        "the report's byte count is not what is on disk ({members:?})"
                    );
                }
            }
        }
        Err(_) => {
            // Refused: whatever was written before the refusal still obeys
            // the root, and the budget was not exceeded on disk either.
            if root.exists() {
                let (_, bytes) = check_tree(&root, &canary);
                assert!(
                    bytes <= policy.max_extracted_bytes,
                    "{bytes} bytes on disk after a refusal"
                );
            }
        }
    }
});
