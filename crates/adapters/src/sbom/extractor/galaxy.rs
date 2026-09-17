use batlehub_core::ports::ExtractedManifest;
use bytes::Bytes;

use super::readme;

/// A collection tarball is a gzipped tar whose root holds `MANIFEST.json`,
/// `FILES.json` and the collection's own tree.
///
/// **The README is named, not guessed.** `MANIFEST.json`'s
/// `collection_info.readme` is a required field for every collection
/// `ansible-galaxy collection build` produces, and it names a file relative to
/// the collection root — conventionally `README.md`, but a collection is free
/// to say `docs/README.rst`. Reading the declared name rather than matching a
/// filename is what keeps this from showing the wrong document, which is the
/// caveat RubyGems' extractor has to carry because a gemspec declares nothing.
///
/// Dependencies and the licence are deliberately **not** read here: they are on
/// the version document this proxy already fetches to resolve a version, and
/// reading them a second time out of the artifact would be a second answer to a
/// question that already has one.
pub(super) fn extract_galaxy_manifest(data: &Bytes) -> ExtractedManifest {
    let Some(name) = declared_readme(data) else {
        // No `MANIFEST.json`, or no `readme` in it. Falling back to a filename
        // match would report a README for a collection that declared none,
        // which is the guess this extractor exists to avoid.
        return ExtractedManifest::default();
    };
    let wanted = name.trim_start_matches("./").to_owned();
    ExtractedManifest {
        readme: readme::readme_from_targz(data, |path| path.trim_start_matches("./") == wanted),
        ..ExtractedManifest::default()
    }
}

/// `collection_info.readme` out of the tarball's `MANIFEST.json`.
fn declared_readme(data: &Bytes) -> Option<String> {
    use batlehub_core::ports::README_EXTRACT_CEILING;
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let mut archive = Archive::new(GzDecoder::new(data.as_ref()));
    for entry in archive.entries().ok()?.flatten() {
        let Ok(path) = entry.path() else { continue };
        let path = path.to_string_lossy().into_owned();
        if !readme::is_inside_root(&path) || path.trim_start_matches("./") != "MANIFEST.json" {
            continue;
        }
        let mut buf = Vec::new();
        entry
            .take(README_EXTRACT_CEILING as u64)
            .read_to_end(&mut buf)
            .ok()?;
        let manifest: serde_json::Value = serde_json::from_slice(&buf).ok()?;
        let name = manifest
            .get("collection_info")?
            .get("readme")?
            .as_str()?
            .trim();
        return (!name.is_empty()).then(|| name.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::readme::fixtures::targz;
    use super::*;

    fn manifest(readme: &str) -> Vec<u8> {
        serde_json::json!({
            "collection_info": {
                "namespace": "acme",
                "name": "util",
                "version": "1.0.0",
                "readme": readme,
            },
            "format": 1,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn the_readme_manifest_json_names_is_the_one_read() {
        let m = manifest("README.md");
        let tarball = targz(&[
            ("MANIFEST.json", m.as_slice()),
            ("README.md", b"# acme.util".as_slice()),
            ("FILES.json", b"{}".as_slice()),
        ]);
        let readme = extract_galaxy_manifest(&tarball)
            .readme
            .expect("README read");
        assert_eq!(readme.content, "# acme.util");
        assert_eq!(readme.path, "README.md");
    }

    /// The reason the name is read rather than matched: a collection may put
    /// its README anywhere, and a filename match would show the wrong file.
    #[test]
    fn a_readme_somewhere_else_is_still_found_by_name() {
        let m = manifest("docs/overview.rst");
        let tarball = targz(&[
            ("MANIFEST.json", m.as_slice()),
            ("README.md", b"not the declared one".as_slice()),
            ("docs/overview.rst", b"Overview\n========".as_slice()),
        ]);
        let readme = extract_galaxy_manifest(&tarball)
            .readme
            .expect("README read");
        assert_eq!(readme.content, "Overview\n========");
        assert_eq!(readme.path, "docs/overview.rst");
    }

    #[test]
    fn a_collection_that_declares_no_readme_reports_none() {
        let m = serde_json::json!({ "collection_info": { "namespace": "a", "name": "b", "version": "1.0.0" } })
            .to_string()
            .into_bytes();
        let tarball = targz(&[
            ("MANIFEST.json", m.as_slice()),
            ("README.md", b"# present but undeclared".as_slice()),
        ]);
        assert!(extract_galaxy_manifest(&tarball).readme.is_none());
    }

    #[test]
    fn a_declared_readme_that_is_not_in_the_archive_reports_none() {
        let m = manifest("README.md");
        let tarball = targz(&[("MANIFEST.json", m.as_slice())]);
        assert!(extract_galaxy_manifest(&tarball).readme.is_none());
    }

    #[test]
    fn a_body_that_is_not_a_collection_yields_nothing() {
        let data = Bytes::from_static(b"not a tarball");
        assert_eq!(extract_galaxy_manifest(&data), ExtractedManifest::default());
    }

    /// A `readme` that walks out of the archive names nothing inside it, so it
    /// finds nothing — `is_inside_root` is checked on the *entry*, and the
    /// comparison is against the declared name, not a join.
    #[test]
    fn a_readme_field_that_walks_finds_nothing() {
        let m = manifest("../../etc/passwd");
        let tarball = targz(&[
            ("MANIFEST.json", m.as_slice()),
            ("README.md", b"# acme.util".as_slice()),
        ]);
        assert!(extract_galaxy_manifest(&tarball).readme.is_none());
    }
}
