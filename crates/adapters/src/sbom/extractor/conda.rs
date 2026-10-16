use batlehub_core::entities::ReadmeFormat;
use batlehub_core::ports::{ExtractedManifest, ExtractedReadme};
use bytes::Bytes;

use super::readme;

/// conda packages carry no README file. What they have is `info/about.json`'s
/// `description`, which is the long text a channel's own page renders — so that
/// is what this returns, as **plain text**: nothing declares it as markdown, and
/// parsing prose as markup mangles indentation and swallows underscores.
///
/// Two container formats, both in the wild:
///
/// - `.tar.bz2`, the original: a bzip2'd tar with `info/about.json` inside.
/// - `.conda`, the current one: a zip holding `info-{name}.tar.zst` and
///   `pkg-{name}.tar.zst`. Only the `info-` member is opened; the payload is the
///   package's files and has nothing to say here.
pub(super) fn extract_conda_manifest(data: &Bytes) -> ExtractedManifest {
    ExtractedManifest {
        readme: about_json(data).and_then(|json| description_readme(&json)),
        ..ExtractedManifest::default()
    }
}

/// `info/about.json`, from whichever container this is.
fn about_json(data: &Bytes) -> Option<String> {
    from_tar_bz2(data).or_else(|| from_conda_zip(data))
}

/// The path `info/about.json` inside a bzip2'd tar.
fn from_tar_bz2(data: &Bytes) -> Option<String> {
    use bzip2::read::BzDecoder;
    use tar::Archive;

    let mut archive = Archive::new(BzDecoder::new(data.as_ref()));
    read_about_from_tar(&mut archive)
}

/// The `info-*.tar.zst` member of a `.conda` zip, then the same path inside it.
fn from_conda_zip(data: &Bytes) -> Option<String> {
    use std::io::{Cursor, Read};
    use tar::Archive;
    use zip::ZipArchive;

    use batlehub_core::ports::README_EXTRACT_CEILING;

    let mut zip = ZipArchive::new(Cursor::new(data.as_ref())).ok()?;
    let member = (0..zip.len())
        .filter_map(|i| {
            zip.by_index(i)
                .ok()?
                .name()
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
        .find(|name| name.starts_with("info-") && name.ends_with(".tar.zst"))?;

    // **Both** decompressions are bounded, and each one on its own output.
    //
    // The ceiling on `read_about_from_tar` bounds only the third stage, which is
    // two stages too late: a `.conda` is a zip (deflate) holding a zstd frame
    // holding a tar, and either of the first two will happily inflate a few
    // megabytes of attacker-supplied bytes into tens of gigabytes before the tar
    // is ever walked. The input is a package body from an upstream, so it is
    // attacker-controlled by construction.
    //
    // `README_EXTRACT_CEILING` is the right bound for both: whatever comes out
    // of them exists only to have `info/about.json` read out of it, and that
    // read is capped at the same number. `take` is on the *decompressed* side,
    // which is the number that matters.
    let mut compressed = Vec::new();
    zip.by_name(&member)
        .inspect_err(|e| tracing::warn!(error = %e, member = %member, "sbom: failed to read conda info member, no README"))
        .ok()?
        .take(README_EXTRACT_CEILING as u64)
        .read_to_end(&mut compressed)
        .ok()?;

    // The read's *error* is discarded, its output is not. Two ways this ends
    // short of a complete frame, and neither is a reason to answer "no
    // `about.json`": the `take` above may have cut the zstd frame mid-way, and
    // the `take` below stops the decoder at the ceiling. `read_to_end` reports
    // the truncation as an error while leaving everything it did decode in
    // `decoded` — and `info/about.json` sorts near the front of the `info/`
    // tar, so it is almost always already in there. `read_about_from_tar` walks
    // what arrived and returns `None` of its own accord if the entry is not in
    // it, which is the honest answer for a package genuinely over the bound.
    let mut decoded = Vec::new();
    let _ = zstd::stream::read::Decoder::new(Cursor::new(compressed))
        .ok()?
        .take(README_EXTRACT_CEILING as u64)
        .read_to_end(&mut decoded);
    if decoded.is_empty() {
        return None;
    }

    let mut archive = Archive::new(Cursor::new(decoded));
    read_about_from_tar(&mut archive)
}

fn read_about_from_tar<R: std::io::Read>(archive: &mut tar::Archive<R>) -> Option<String> {
    use batlehub_core::ports::README_EXTRACT_CEILING;
    use std::io::Read;

    let entries = archive.entries().ok()?;
    for entry in entries.flatten() {
        let Ok(path) = entry.path() else { continue };
        let path = path.to_string_lossy().into_owned();
        if !readme::is_inside_root(&path) || path.trim_start_matches("./") != "info/about.json" {
            continue;
        }
        let mut buf = String::new();
        // Bounded like every other archive read: `about.json` is small in
        // practice and attacker-controlled in principle.
        if entry
            .take(README_EXTRACT_CEILING as u64)
            .read_to_string(&mut buf)
            .is_ok()
        {
            return Some(buf);
        }
        return None;
    }
    None
}

fn description_readme(json: &str) -> Option<ExtractedReadme> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = value.get("description")?.as_str()?;
    readme::from_manifest_field(text, "info/about.json", ReadmeFormat::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_becomes_a_plain_text_readme() {
        let found = description_readme(r#"{"description":"A conda package.","home":"x"}"#).unwrap();
        assert_eq!(found.content, "A conda package.");
        // Plain, not markdown: nothing declared it as markup, and parsing prose
        // as markdown mangles indentation and swallows underscores.
        assert_eq!(found.format, ReadmeFormat::Plain);
        assert_eq!(found.path, "info/about.json");
    }

    #[test]
    fn an_absent_or_empty_description_is_no_readme() {
        assert!(description_readme(r#"{"home":"x"}"#).is_none());
        assert!(description_readme(r#"{"description":""}"#).is_none());
        assert!(description_readme(r#"{"description":"  \n "}"#).is_none());
        assert!(description_readme(r#"{"description":null}"#).is_none());
        // A malformed `about.json` is a package we cannot describe, not an error.
        assert!(description_readme("not json").is_none());
    }

    /// Neither container: a body that is not an archive at all returns nothing
    /// rather than erroring.
    #[test]
    fn a_non_archive_body_yields_nothing() {
        let data = Bytes::from_static(b"not an archive");
        assert_eq!(extract_conda_manifest(&data), ExtractedManifest::default());
    }

    /// The original container: a bzip2'd tar with `info/about.json` inside.
    #[test]
    fn a_tar_bz2_packages_description_is_read() {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;

        let about = br#"{"description":"A conda package.\n\nWith a second paragraph."}"#;
        let mut builder = tar::Builder::new(BzEncoder::new(Vec::new(), Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_size(about.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "info/about.json", about.as_slice())
            .unwrap();
        let data = Bytes::from(builder.into_inner().unwrap().finish().unwrap());

        let readme = extract_conda_manifest(&data)
            .readme
            .expect("description read");
        assert_eq!(
            readme.content,
            "A conda package.\n\nWith a second paragraph."
        );
        assert_eq!(readme.format, ReadmeFormat::Plain);
    }

    /// The current container: a zip holding `info-*.tar.zst` and `pkg-*.tar.zst`.
    /// Only the `info-` member is opened — the payload is the package's files
    /// and has nothing to say here.
    #[test]
    fn a_conda_zips_info_member_is_the_one_that_is_opened() {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let inner = |json: &[u8]| {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "info/about.json", json)
                .unwrap();
            zstd::stream::encode_all(Cursor::new(builder.into_inner().unwrap()), 1).unwrap()
        };

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("pkg-x-1.0.tar.zst", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(&inner(br#"{"description":"the payload, not the answer"}"#))
            .unwrap();
        writer
            .start_file("info-x-1.0.tar.zst", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(&inner(br#"{"description":"the real description"}"#))
            .unwrap();
        let data = Bytes::from(writer.finish().unwrap().into_inner());

        assert_eq!(
            extract_conda_manifest(&data).readme.unwrap().content,
            "the real description"
        );
    }
}
