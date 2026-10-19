//! Reading a README out of a package archive, safely and once.
//!
//! Shared by every archive-borne registry kind so the three guards RFC 0007 §7.5
//! requires cannot be implemented differently in nine places:
//!
//! - at most [`README_EXTRACT_CEILING`] **decompressed** bytes from the single
//!   entry wanted — the input is attacker-controlled and compresses well;
//! - an entry whose path escapes the archive root is refused outright, so a
//!   crafted `../../etc/passwd` member cannot be the document a panel shows;
//! - nothing is written to disk, and non-UTF-8 bytes are not a README.

use std::io::Read;

use batlehub_core::entities::ReadmeFormat;
use batlehub_core::ports::{ExtractedReadme, README_EXTRACT_CEILING};
use bytes::Bytes;

use batlehub_core::services::readme::detect;

/// Whether an archive member path stays inside the archive root.
///
/// `..` anywhere, an absolute path, or a Windows drive prefix all escape. A
/// README is only ever read out of an archive, never written, so this cannot
/// overwrite anything — but a member claiming to be `../../../README.md` is a
/// member claiming to be a file it is not, and showing its contents as *this
/// package's* documentation would be wrong even if it is harmless.
pub(super) fn is_inside_root(path: &str) -> bool {
    !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains(':')
        && !path
            .split(['/', '\\'])
            .any(|segment| segment == ".." || segment == "~")
}

/// Decompressed bytes read out of one `.tar.gz` before an extractor gives up —
/// helm's default chart limit, which `registry::helm::archive` applies at
/// publish. Skipping an entry still inflates it, and `tar` buffers a GNU
/// long-name or pax entry whole whatever its header claims, so without a bound
/// on the stream a few MB of upload is gigabytes of work or of memory here, in
/// a background task nobody watches.
const MAX_DECOMPRESSED_BYTES: u64 = 100 * 1024 * 1024;

/// A `.tar.gz` held in memory, with its decompressed stream bounded. Every
/// extractor that walks one opens it here.
pub(super) fn open_targz(
    data: &[u8],
) -> tar::Archive<std::io::Take<flate2::read::GzDecoder<&[u8]>>> {
    tar::Archive::new(flate2::read::GzDecoder::new(data).take(MAX_DECOMPRESSED_BYTES))
}

/// Read at most [`README_EXTRACT_CEILING`] bytes and turn them into a README.
///
/// `None` when the bytes are not UTF-8: a document full of replacement
/// characters is worse than saying there is none, because a reader cannot tell
/// which characters were the package's.
fn read_bounded(mut reader: impl Read, path: &str) -> Option<ExtractedReadme> {
    let mut buf = Vec::new();
    // `take` bounds the *decompressed* read, which is the number that matters:
    // `read_to_end` on a decompressor is unbounded by the compressed size.
    // One extra byte, so hitting the ceiling exactly is distinguishable from
    // running past it.
    if reader
        .by_ref()
        .take(README_EXTRACT_CEILING as u64 + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        tracing::warn!(path, "readme: archive entry could not be read");
        return None;
    }
    let truncated = buf.len() > README_EXTRACT_CEILING;
    if truncated {
        buf.truncate(README_EXTRACT_CEILING);
        // Truncation may have landed mid-character; drop the partial one rather
        // than failing the whole read over it.
        //
        // Exactly the *trailing* partial sequence, in one pass. Popping a byte at
        // a time until the whole buffer validates looks equivalent and is not:
        // `from_utf8` scans to the first bad byte on every call, so an entry with
        // an invalid byte in the middle makes this quadratic — a 4 MiB buffer with
        // a bad byte at 2 MiB costs ~4×10¹² byte comparisons and pins a runtime
        // worker for minutes, on input an upstream chooses. `error_len() == None`
        // is precisely "input ended mid-sequence", which is the only case
        // truncation can create; anything else is a genuinely invalid byte and
        // must still fail the read below rather than be eroded away.
        if let Err(e) = std::str::from_utf8(&buf) {
            if e.error_len().is_none() {
                buf.truncate(e.valid_up_to());
            }
        }
    }
    let content = match String::from_utf8(buf) {
        Ok(text) => text,
        Err(_) => {
            tracing::warn!(path, "readme: archive entry is not valid UTF-8; ignoring");
            return None;
        }
    };
    if content.trim().is_empty() {
        return None;
    }
    Some(ExtractedReadme {
        format: detect::format_from_filename(path),
        content,
        path: path.to_owned(),
        truncated,
    })
}

/// The first entry of a gzipped tar whose path `want` accepts.
///
/// `want` receives the archive-relative path, already checked for escape.
pub(super) fn readme_from_targz(
    data: &Bytes,
    want: impl Fn(&str) -> bool,
) -> Option<ExtractedReadme> {
    let mut archive = open_targz(data);
    let entries = archive.entries().ok()?;
    for entry in entries.flatten() {
        let Ok(path) = entry.path() else { continue };
        let path = path.to_string_lossy().into_owned();
        if !is_inside_root(&path) || !want(&path) {
            continue;
        }
        return read_bounded(entry, &path);
    }
    None
}

/// The first entry of a zip whose path `want` accepts.
pub(super) fn readme_from_zip(
    data: &Bytes,
    want: impl Fn(&str) -> bool,
) -> Option<ExtractedReadme> {
    use std::io::Cursor;
    use zip::ZipArchive;

    let mut archive = ZipArchive::new(Cursor::new(data.as_ref())).ok()?;
    // Two passes over the index rather than one, because `by_index` borrows the
    // archive mutably and the name has to outlive that borrow.
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive
                .by_index(i)
                .ok()?
                .name()
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
        .collect();
    let wanted = names
        .into_iter()
        .find(|name| is_inside_root(name) && want(name))?;
    let file = archive.by_name(&wanted).ok()?;
    read_bounded(file, &wanted)
}

/// Read one named entry out of a zip, for the kinds whose manifest *names* the
/// README file (NuGet's `.nuspec` `<readme>`).
pub(super) fn zip_entry(data: &Bytes, path: &str) -> Option<ExtractedReadme> {
    let wanted = path.trim_start_matches("./").replace('\\', "/");
    readme_from_zip(data, |name| name.replace('\\', "/") == wanted)
}

/// The conventional `README*` at a given depth from the archive root.
///
/// `depth` is the number of path segments before the filename: `0` for an
/// archive whose files sit at the root, `1` for the single wrapper directory
/// npm (`package/`), cargo (`{name}-{version}/`) and Go module zips use.
pub(super) fn root_readme_matcher(depth: usize) -> impl Fn(&str) -> bool {
    move |path: &str| {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        segments.len() == depth + 1
            && segments
                .last()
                .is_some_and(|name| detect::is_readme_filename(name))
    }
}

/// The `README*` closest to the archive root, wherever that is.
///
/// The wrapper directory is not a fixed depth for every kind: a Go module zip
/// prefixes every path with `{module}@{version}/`, and the module path itself
/// contains slashes (`github.com/user/repo@v1.0.0/README.md`); a Composer dist
/// zip's prefix is a GitHub archive's `{vendor}-{repo}-{sha}`, or nothing at all
/// when it was built by `composer archive`.
///
/// Shallowest rather than first, because "first" is archive-order and would let
/// `guide/subproject/README.md` win over the package's own. Ties keep the first
/// seen, which is the archive's own order and is as good an answer as any when
/// two READMEs sit at the same depth.
pub(super) fn shallowest_readme(data: &Bytes, is_zip: bool) -> Option<ExtractedReadme> {
    let candidates = if is_zip {
        zip_paths(data)
    } else {
        targz_paths(data)
    };
    let best = candidates
        .into_iter()
        .filter(|path| is_inside_root(path) && is_readme_path(path))
        .min_by_key(|path| path.split('/').filter(|s| !s.is_empty()).count())?;

    if is_zip {
        readme_from_zip(data, |name| name == best)
    } else {
        readme_from_targz(data, |name| name == best)
    }
}

/// Whether a path's final segment is the conventional README name.
fn is_readme_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(detect::is_readme_filename)
}

/// Every member path of a zip, without reading any bodies.
fn zip_paths(data: &Bytes) -> Vec<String> {
    use std::io::Cursor;
    use zip::ZipArchive;

    let Ok(mut archive) = ZipArchive::new(Cursor::new(data.as_ref())) else {
        return Vec::new();
    };
    (0..archive.len())
        .filter_map(|i| {
            archive
                .by_index(i)
                .ok()?
                .name()
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
        .collect()
}

/// Every member path of a gzipped tar, without reading any bodies.
fn targz_paths(data: &Bytes) -> Vec<String> {
    let mut archive = open_targz(data);
    let Ok(entries) = archive.entries() else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
        .collect()
}

/// A README built from a plain string a manifest carried, rather than a file.
///
/// conda's `info/about.json` has a `description` field; there is no README file
/// in a `.conda` package at all, so the "path" recorded is the manifest that
/// declared it — which is what an operator checking where the text came from
/// needs to see.
pub(super) fn from_manifest_field(
    text: &str,
    path: &str,
    format: ReadmeFormat,
) -> Option<ExtractedReadme> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let truncated = trimmed.len() > README_EXTRACT_CEILING;
    let content = if truncated {
        let mut end = README_EXTRACT_CEILING;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        trimmed[..end].to_owned()
    } else {
        trimmed.to_owned()
    };
    Some(ExtractedReadme {
        content,
        format,
        path: path.to_owned(),
        truncated,
    })
}

/// Archive builders, so the extractor tests exercise real containers rather
/// than a mock of one. RFC 0009's lesson was that tests written from our
/// implementation rather than from what the client sends pass while the code is
/// wrong; a README extractor tested against a fake archive would be the same
/// mistake in miniature.
#[cfg(test)]
pub(super) mod fixtures {
    use bytes::Bytes;

    /// A gzipped tar of `(path, body)` pairs.
    pub(in crate::sbom::extractor) fn targz(entries: &[(&str, &[u8])]) -> Bytes {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (path, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        Bytes::from(builder.into_inner().unwrap().finish().unwrap())
    }

    /// A plain (uncompressed) tar, which is what a `.gem` is.
    pub(in crate::sbom::extractor) fn plain_tar(entries: &[(&str, &[u8])]) -> Bytes {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        Bytes::from(builder.into_inner().unwrap())
    }

    /// A zip of `(path, body)` pairs.
    pub(in crate::sbom::extractor) fn zipped(entries: &[(&str, &[u8])]) -> Bytes {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, body) in entries {
            writer
                .start_file(*path, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        Bytes::from(writer.finish().unwrap().into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn an_entry_that_escapes_the_root_is_refused() {
        assert!(is_inside_root("package/README.md"));
        assert!(is_inside_root("README"));
        assert!(!is_inside_root("../README.md"));
        assert!(!is_inside_root("package/../../README.md"));
        assert!(!is_inside_root("/etc/passwd"));
        assert!(!is_inside_root("C:/Windows/README.md"));
        assert!(!is_inside_root("~/README.md"));
        // A backslash separator is still a separator on the way in.
        assert!(!is_inside_root("package\\..\\README.md"));
    }

    #[test]
    fn the_root_matcher_takes_the_wrapper_directorys_readme_and_no_deeper_one() {
        let at_one = root_readme_matcher(1);
        assert!(at_one("package/README.md"));
        assert!(at_one("mylib-1.0.0/readme"));
        assert!(!at_one("README.md"));
        assert!(!at_one("package/guide/README.md"));
        assert!(!at_one("package/CHANGELOG.md"));

        let at_root = root_readme_matcher(0);
        assert!(at_root("README.rst"));
        assert!(!at_root("package/README.md"));
    }

    #[test]
    fn a_manifest_field_becomes_a_readme_unless_it_is_blank() {
        let found = from_manifest_field("A conda package.", "info/about.json", ReadmeFormat::Plain)
            .unwrap();
        assert_eq!(found.content, "A conda package.");
        assert_eq!(found.path, "info/about.json");
        assert_eq!(found.format, ReadmeFormat::Plain);
        assert!(!found.truncated);

        assert!(from_manifest_field("   \n ", "info/about.json", ReadmeFormat::Plain).is_none());
    }

    /// The ceiling is on decompressed bytes and the cut lands on a character
    /// boundary — a `String` cannot hold half a code point.
    #[test]
    fn an_oversized_manifest_field_is_cut_on_a_character_boundary() {
        let long = "é".repeat(README_EXTRACT_CEILING);
        let found = from_manifest_field(&long, "info/about.json", ReadmeFormat::Plain).unwrap();
        assert!(found.truncated);
        assert!(found.content.len() <= README_EXTRACT_CEILING);
        // Round-tripping proves nothing was cut mid-character.
        assert!(found.content.chars().all(|c| c == 'é'));
    }

    /// The conventional README under a single wrapper directory — the shape npm
    /// and cargo archives have.
    #[test]
    fn a_wrapped_readme_is_found_with_its_declared_markup() {
        let data = targz(&[
            ("package/package.json", b"{}"),
            ("package/README.md", b"# hello"),
        ]);
        let found = readme_from_targz(&data, root_readme_matcher(1)).unwrap();
        assert_eq!(found.content, "# hello");
        assert_eq!(found.path, "package/README.md");
        assert_eq!(found.format, ReadmeFormat::Markdown);
        assert!(!found.truncated);
    }

    #[test]
    fn decompression_stops_at_the_archive_ceiling() {
        // 101 MiB of zeros ahead of the README: a few hundred KB on the wire,
        // and past `MAX_DECOMPRESSED_BYTES`, so the walk ends before it.
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(101 * 1024 * 1024);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(
            &mut header,
            "mychart/big",
            std::io::Read::take(std::io::repeat(0), 101 * 1024 * 1024),
        )
        .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "mychart/README.md", b"# chart".as_slice())
            .unwrap();
        let data = Bytes::from(tar.into_inner().unwrap().finish().unwrap());
        assert!(readme_from_targz(&data, root_readme_matcher(1)).is_none());
    }

    /// An entry whose path escapes the archive root is refused, so a crafted
    /// member cannot be the document a panel shows as this package's.
    #[test]
    fn an_entry_that_escapes_the_root_is_never_the_readme() {
        // `tar` normalises `..` out of the *path it writes*, so the escape is
        // asserted at the matcher, which is what sees an already-parsed path.
        let matcher = root_readme_matcher(1);
        for path in ["../README.md", "/etc/README", "pkg/../../README.md"] {
            assert!(
                !(is_inside_root(path) && matcher(path)),
                "{path} must not be accepted"
            );
        }
    }

    /// A README the archive has no top-level copy of is not substituted from a
    /// subdirectory: `vendored/dep/README.md` describes something else.
    #[test]
    fn a_deeper_readme_is_not_mistaken_for_the_packages_own() {
        let data = targz(&[("package/vendored/dep/README.md", b"# not ours")]);
        assert!(readme_from_targz(&data, root_readme_matcher(1)).is_none());
    }

    /// The shallowest wins, whatever the archive's own order — "first" would be
    /// archive-order and would let a vendored README beat the real one.
    #[test]
    fn the_shallowest_readme_wins_regardless_of_archive_order() {
        let data = zipped(&[
            ("repo-abc123/guide/README.md", b"# the deep one"),
            ("repo-abc123/README.md", b"# the real one"),
        ]);
        assert_eq!(
            shallowest_readme(&data, true).unwrap().content,
            "# the real one"
        );
    }

    /// A Go module zip's prefix contains slashes, so the wrapper depth is not
    /// fixed and a depth-N matcher would miss it entirely.
    #[test]
    fn a_go_module_zips_deep_prefix_is_still_the_root() {
        let data = zipped(&[
            ("github.com/user/repo@v1.2.3/go.mod", b"module x"),
            ("github.com/user/repo@v1.2.3/README.md", b"# the module"),
        ]);
        assert_eq!(
            shallowest_readme(&data, true).unwrap().content,
            "# the module"
        );
    }

    /// An entry longer than the ceiling is truncated and flagged, never silently
    /// shortened — and never read past the ceiling into memory.
    #[test]
    fn an_oversized_entry_is_truncated_and_flagged() {
        let huge = vec![b'a'; README_EXTRACT_CEILING + 1024];
        let data = targz(&[("package/README.md", &huge)]);
        let found = readme_from_targz(&data, root_readme_matcher(1)).unwrap();
        assert!(found.truncated);
        assert_eq!(found.content.len(), README_EXTRACT_CEILING);
    }

    /// A README that is not UTF-8 is not a README this can display: a document
    /// full of replacement characters is worse than saying there is none,
    /// because a reader cannot tell which characters were the package's.
    #[test]
    fn a_non_utf8_entry_is_not_a_readme() {
        let data = targz(&[("package/README.md", &[0xff, 0xfe, 0xff])]);
        assert!(readme_from_targz(&data, root_readme_matcher(1)).is_none());
    }

    /// An empty or whitespace-only file is nothing, not a document.
    #[test]
    fn an_empty_entry_is_not_a_readme() {
        let data = targz(&[("package/README.md", b"   \n\t\n")]);
        assert!(readme_from_targz(&data, root_readme_matcher(1)).is_none());
    }

    /// A body that is not an archive at all returns nothing rather than
    /// erroring: the caller is a detached best-effort task, and a `.tgz` that is
    /// really an HTML error page is a thing upstreams serve.
    #[test]
    fn a_non_archive_body_yields_nothing() {
        let data = Bytes::from_static(b"<html>404</html>");
        assert!(readme_from_targz(&data, root_readme_matcher(1)).is_none());
        assert!(readme_from_zip(&data, root_readme_matcher(1)).is_none());
        assert!(shallowest_readme(&data, true).is_none());
    }

    /// A named entry, for the kinds whose manifest points at one file.
    #[test]
    fn a_named_zip_entry_is_read_by_its_declared_path() {
        let data = zipped(&[
            ("pkg.nuspec", b"<package/>"),
            ("guide/README.md", b"# nested"),
        ]);
        assert_eq!(
            zip_entry(&data, "guide/README.md").unwrap().content,
            "# nested"
        );
        // The declared path is normalised the way manifests write it.
        assert_eq!(
            zip_entry(&data, "guide\\README.md").unwrap().content,
            "# nested"
        );
        assert!(zip_entry(&data, "guide/MISSING.md").is_none());
        // A declared path that escapes the archive is refused, not resolved.
        assert!(zip_entry(&data, "../README.md").is_none());
    }
}
