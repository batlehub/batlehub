use batlehub_core::ports::{ExtractedManifest, SbomDependency};
use bytes::Bytes;

use super::{anchor, readme};

pub(super) fn extract_nuget_manifest(data: &Bytes) -> ExtractedManifest {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let cursor = Cursor::new(data.as_ref());
    let Ok(mut archive) = ZipArchive::new(cursor) else {
        tracing::warn!("sbom: failed to parse nuget manifest, treating as no dependencies");
        return ExtractedManifest::default();
    };

    // The single `{id}.nuspec` at the archive root, where NuGet puts it and
    // where NuGet allows exactly one. `ends_with(".nuspec")` matched one at any
    // depth and took the first, so a second `.nuspec` decided the answer. See
    // `anchor`.
    let matches: Vec<usize> = (0..archive.len())
        .filter(|i| {
            archive
                .by_index(*i)
                .is_ok_and(|f| f.name().is_ok_and(|n| anchor::is_nuspec_manifest(&n)))
        })
        .collect();
    let Some(index) = anchor::sole(matches) else {
        return ExtractedManifest::default();
    };

    let Ok(mut file) = archive.by_index(index) else {
        return ExtractedManifest::default();
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        tracing::warn!("sbom: failed to parse nuget manifest, treating as no dependencies");
        return ExtractedManifest::default();
    }
    let mut manifest = parse_nuspec(&content);
    // `<readme>guide/README.md</readme>` names a file inside the `.nupkg`. There
    // is no convention to fall back to: NuGet requires the element, and a
    // package without it has none.
    manifest.readme = nuspec_readme_path(&content).and_then(|path| readme::zip_entry(data, &path));
    manifest
}

fn parse_nuget_dep_from_empty(e: &quick_xml::events::BytesStart<'_>) -> Option<SbomDependency> {
    let mut id = String::new();
    let mut version = String::new();
    for attr in e.attributes().flatten() {
        let kn = attr.key.local_name();
        let key = kn.as_ref();
        // quick-xml 0.42 decodes in the reader, so there is no `Decoder` to
        // thread through here any more — normalisation is all that is left.
        let val = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_default();
        match key {
            "id" => id = val,
            "version" => version = val,
            _ => {}
        }
    }
    if id.is_empty() {
        return None;
    }
    Some(SbomDependency {
        name: id,
        version_req: if version.is_empty() {
            None
        } else {
            Some(version)
        },
        ecosystem: "nuget".into(),
    })
}

fn parse_nuspec(content: &str) -> ExtractedManifest {
    use quick_xml::{events::Event, Reader};

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut deps = Vec::new();

    // `<license type="expression">MIT</license>` is the modern form.
    // `<licenseUrl>` is deprecated and is a URL, not an expression, so it is
    // not read: handing the gate `https://…/LICENSE` would look like a
    // declaration and match nothing.
    let mut capture_license = false;
    let mut license: Option<String> = None;

    // <dependency> elements in .nuspec are always self-closing:
    //   <dependency id="Newtonsoft.Json" version="[13.0,)" />
    /// A self-closing `<dependency …/>` yields one dependency, or nothing.
    fn on_empty(e: &quick_xml::events::BytesStart<'_>, deps: &mut Vec<SbomDependency>) {
        if e.local_name().as_ref() != "dependency" {
            return;
        }
        if let Some(dep) = parse_nuget_dep_from_empty(e) {
            deps.push(dep);
        }
    }

    /// The text of a `<license>` element, when there is something to assign.
    ///
    /// `None` for a blank one, so the caller's `or` leaves an earlier value
    /// alone rather than clearing it.
    fn license_text(e: &quick_xml::events::BytesText<'_>) -> Option<String> {
        Some(e.trim().to_owned()).filter(|text| !text.is_empty())
    }

    /// `type="file"` points at a file inside the package, same problem as
    /// `licenseUrl`; only an expression is read.
    fn opens_license_expression(e: &quick_xml::events::BytesStart<'_>) -> bool {
        if e.local_name().as_ref() != "license" {
            return false;
        }
        !e.attributes()
            .flatten()
            .any(|attr| attr.key.local_name().as_ref() == "type" && attr.value.as_ref() == "file")
    }

    loop {
        match reader.read_event() {
            Ok(Event::Empty(ref e)) => on_empty(e, &mut deps),
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == "license" => {
                capture_license = opens_license_expression(e);
            }
            Ok(Event::Text(ref e)) if capture_license => {
                capture_license = false;
                // `or`, not an assignment: a blank or undecodable `<license>`
                // keeps whatever an earlier one set rather than clearing it.
                license = license_text(e).or(license);
            }
            Ok(Event::End(ref e)) if e.local_name().as_ref() == "license" => {
                capture_license = false;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    ExtractedManifest {
        dependencies: deps,
        license,
        readme: None,
    }
}

/// The path `<readme>` names, if the `.nuspec` has one.
///
/// A second small pass over the same string rather than a fourth piece of state
/// threaded through `parse_nuspec`'s loop: that loop already tracks two capture
/// flags, and the README is not part of the SBOM answer.
fn nuspec_readme_path(content: &str) -> Option<String> {
    use quick_xml::{events::Event, Reader};

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut capture = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                capture = e.local_name().as_ref() == "readme";
            }
            Ok(Event::Text(ref e)) if capture => {
                let text = e.trim();
                return (!text.is_empty()).then(|| text.to_owned());
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => capture = false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_NUSPEC: &[u8] =
        br#"<package><metadata><license type="expression">GPL-3.0-only</license></metadata></package>"#;
    const DECOY_NUSPEC: &[u8] =
        br#"<package><metadata><license type="expression">MIT</license></metadata></package>"#;

    /// A `.nuspec` below the root is not the package's manifest — NuGet puts
    /// exactly one at the root — but `ends_with(".nuspec")` accepted it and zip
    /// order made it win.
    #[test]
    fn a_nested_nuspec_is_not_the_packages_manifest() {
        let data = zipped(&[
            ("lib/net8.0/decoy.nuspec", DECOY_NUSPEC),
            ("mylib.nuspec", REAL_NUSPEC),
        ]);
        assert_eq!(
            extract_nuget_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    /// Two at the root is malformed — NuGet itself rejects it — so the answer is
    /// unknown rather than whichever the attacker put first.
    #[test]
    fn two_root_nuspecs_are_ambiguous_not_first_wins() {
        let data = zipped(&[("aaa.nuspec", DECOY_NUSPEC), ("mylib.nuspec", REAL_NUSPEC)]);
        assert_eq!(extract_nuget_manifest(&data), ExtractedManifest::default());
    }

    #[test]
    fn parse_nuspec_deps_basic() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
  <metadata>
    <id>MyLib</id>
    <version>1.0.0</version>
    <dependencies>
      <group targetFramework="net6.0">
        <dependency id="Newtonsoft.Json" version="[13.0,)" />
        <dependency id="Serilog" version="2.12.0" />
      </group>
    </dependencies>
  </metadata>
</package>"#;
        let deps = parse_nuspec(nuspec).dependencies;
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "Newtonsoft.Json");
        assert_eq!(deps[0].version_req.as_deref(), Some("[13.0,)"));
        assert_eq!(deps[0].ecosystem, "nuget");
        assert_eq!(deps[1].name, "Serilog");
        assert_eq!(deps[1].version_req.as_deref(), Some("2.12.0"));
    }

    #[test]
    fn parse_nuspec_deps_no_version() {
        let nuspec = r#"<package><metadata><dependencies>
          <dependency id="SomeLib" />
        </dependencies></metadata></package>"#;
        let deps = parse_nuspec(nuspec).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "SomeLib");
        assert!(deps[0].version_req.is_none());
    }

    #[test]
    fn parse_nuspec_deps_empty_deps() {
        let nuspec = r#"<package><metadata><id>Foo</id></metadata></package>"#;
        let deps = parse_nuspec(nuspec).dependencies;
        assert!(deps.is_empty());
    }

    #[test]
    fn parse_nuspec_license_expression() {
        let nuspec = r#"<package><metadata>
          <license type="expression">MIT</license>
        </metadata></package>"#;
        assert_eq!(parse_nuspec(nuspec).license.as_deref(), Some("MIT"));
    }

    /// `type="file"` names a file inside the package; `licenseUrl` is a URL.
    /// Neither is an expression, and reporting one would look like a
    /// declaration the gate could match.
    #[test]
    fn parse_nuspec_license_ignores_file_and_url_forms() {
        let file_form = r#"<package><metadata>
          <license type="file">LICENSE.txt</license>
        </metadata></package>"#;
        assert_eq!(parse_nuspec(file_form).license, None);

        let url_form = r#"<package><metadata>
          <licenseUrl>https://example.test/LICENSE</licenseUrl>
        </metadata></package>"#;
        assert_eq!(parse_nuspec(url_form).license, None);
    }

    #[test]
    fn parse_nuspec_license_absent_is_none() {
        let nuspec = r#"<package><metadata><id>Foo</id></metadata></package>"#;
        assert_eq!(parse_nuspec(nuspec).license, None);
    }

    fn make_nupkg_with_nuspec(nuspec: &str) -> Bytes {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        zip.start_file("mylib.nuspec", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(nuspec.as_bytes()).unwrap();
        zip.finish().unwrap();
        Bytes::from(buf)
    }

    #[test]
    fn extract_nuget_deps_from_nupkg() {
        let nuspec = r#"<package><metadata><dependencies>
          <dependency id="Newtonsoft.Json" version="13.0.0" />
        </dependencies></metadata></package>"#;
        let data = make_nupkg_with_nuspec(nuspec);
        let deps = extract_nuget_manifest(&data).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "Newtonsoft.Json");
    }

    #[test]
    fn extract_nuget_deps_invalid_zip() {
        assert_eq!(
            extract_nuget_manifest(&Bytes::from_static(b"not a zip")),
            ExtractedManifest::default()
        );
    }

    use super::super::readme::fixtures::zipped;

    /// `<readme>` names a file inside the `.nupkg`, and NuGet allows it to be
    /// nested.
    #[test]
    fn the_nuspec_readme_element_names_the_file_that_is_read() {
        let data = zipped(&[
            (
                "pkg.nuspec",
                br#"<?xml version="1.0"?><package><metadata>
                     <license type="expression">MIT</license>
                     <readme>guide\README.md</readme>
                   </metadata></package>"#
                    .as_slice(),
            ),
            ("guide/README.md", b"# the package".as_slice()),
            ("README.md", b"# a decoy at the root".as_slice()),
        ]);
        let manifest = extract_nuget_manifest(&data);
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        let readme = manifest.readme.expect("README read");
        assert_eq!(readme.content, "# the package");
        assert_eq!(readme.path, "guide/README.md");
    }

    /// NuGet requires the element, so a package without it has none — there is
    /// no convention to fall back to, and a root `README.md` in a `.nupkg` is
    /// as likely to be a packed content file as the package's own.
    #[test]
    fn a_nuspec_without_a_readme_element_reports_none() {
        let data = zipped(&[
            (
                "pkg.nuspec",
                br#"<?xml version="1.0"?><package><metadata/></package>"#.as_slice(),
            ),
            ("README.md", b"# not declared".as_slice()),
        ]);
        assert!(extract_nuget_manifest(&data).readme.is_none());
    }

    /// A `<readme>` naming a file that is not in the package reports none
    /// rather than falling back to something else.
    #[test]
    fn a_readme_element_pointing_at_nothing_reports_none() {
        let data = zipped(&[(
            "pkg.nuspec",
            br#"<?xml version="1.0"?><package><metadata>
                 <readme>MISSING.md</readme>
               </metadata></package>"#
                .as_slice(),
        )]);
        assert!(extract_nuget_manifest(&data).readme.is_none());
    }
}
