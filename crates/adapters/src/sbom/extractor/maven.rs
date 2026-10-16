use batlehub_core::ports::{ExtractedManifest, SbomDependency};
use bytes::Bytes;

pub(super) fn extract_maven_manifest(data: &Bytes) -> ExtractedManifest {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let cursor = Cursor::new(data.as_ref());
    let Ok(mut archive) = ZipArchive::new(cursor) else {
        tracing::warn!("sbom: failed to parse maven manifest, treating as no dependencies");
        return ExtractedManifest::default();
    };

    // `META-INF/maven/{g}/{a}/pom.xml`, and only if there is exactly one.
    // `ends_with("pom.xml")` matched `a/decoypom.xml` — a file not even named
    // `pom.xml` — and took whichever came first. An uber/shaded jar legitimately
    // matches several and now reports unknown rather than some bundled
    // dependency's licence as the artifact's. See `anchor`.
    let matches: Vec<usize> = (0..archive.len())
        .filter(|i| {
            archive
                .by_index(*i)
                .is_ok_and(|f| f.name().is_ok_and(|n| super::anchor::is_maven_manifest(&n)))
        })
        .collect();
    let Some(index) = super::anchor::sole(matches) else {
        return ExtractedManifest::default();
    };

    let mut file = match archive.by_index(index) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(error = %e, "sbom: failed to read maven manifest, treating as no dependencies");
            return ExtractedManifest::default();
        }
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        tracing::warn!("sbom: failed to parse maven manifest, treating as no dependencies");
        return ExtractedManifest::default();
    }
    parse_maven_pom(&content)
}

fn decode_xml_text(e: &quick_xml::events::BytesText) -> String {
    // quick-xml 0.42 validates UTF-8 in the reader and stores event content as
    // `str`, so the charset decode step is gone; only entity unescaping is left.
    let raw: &str = e;
    quick_xml::escape::unescape(raw)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| raw.to_owned())
}

fn finalize_maven_dependency(group: &str, artifact: &str, version: &str) -> Option<SbomDependency> {
    if artifact.is_empty() {
        return None;
    }
    let name = if group.is_empty() {
        artifact.to_owned()
    } else {
        format!("{group}:{artifact}")
    };
    Some(SbomDependency {
        name,
        version_req: if version.is_empty() {
            None
        } else {
            Some(version.to_owned())
        },
        ecosystem: "maven".into(),
    })
}

fn apply_maven_start(local: &str, in_dep: &mut u32, capture: &mut Option<&'static str>) {
    match local {
        "dependency" => *in_dep += 1,
        "groupId" if *in_dep > 0 => *capture = Some("groupId"),
        "artifactId" if *in_dep > 0 => *capture = Some("artifactId"),
        "version" if *in_dep > 0 => *capture = Some("version"),
        _ => {}
    }
}

fn apply_maven_end(
    local: &str,
    in_dep: &mut u32,
    group: &mut String,
    artifact: &mut String,
    version: &mut String,
    deps: &mut Vec<SbomDependency>,
) {
    if local == "dependency" && *in_dep > 0 {
        *in_dep -= 1;
        if let Some(dep) = finalize_maven_dependency(group, artifact, version) {
            deps.push(dep);
        }
        group.clear();
        artifact.clear();
        version.clear();
    }
}

/// The bookkeeping [`parse_maven_pom`] carries from one event to the next.
#[derive(Default)]
struct PomParser {
    deps: Vec<SbomDependency>,
    in_dependency: u32,
    current_group: String,
    current_artifact: String,
    current_version: String,
    capture_field: Option<&'static str>,
    /// `<name>` is also the project's own display name, so the licence capture
    /// is scoped to `<licenses>` rather than matched on the element alone.
    in_licenses: u32,
    capture_license_name: bool,
    licenses: Vec<String>,
}

impl PomParser {
    fn start(&mut self, e: &quick_xml::events::BytesStart<'_>) {
        let ln = e.local_name();
        let local = ln.as_ref();
        apply_maven_start(local, &mut self.in_dependency, &mut self.capture_field);
        if local == "licenses" {
            self.in_licenses += 1;
        } else if local == "name" && self.in_licenses > 0 {
            self.capture_license_name = true;
        }
    }

    fn text(&mut self, e: &quick_xml::events::BytesText<'_>) {
        if self.capture_license_name {
            self.capture_license_name = false;
            let text = decode_xml_text(e);
            let text = text.trim();
            if !text.is_empty() {
                self.licenses.push(text.to_owned());
            }
            return;
        }
        let Some(field) = self.capture_field.take() else {
            return;
        };
        let text = decode_xml_text(e);
        match field {
            "groupId" => self.current_group = text,
            "artifactId" => self.current_artifact = text,
            "version" => self.current_version = text,
            _ => {}
        }
    }

    fn end(&mut self, e: &quick_xml::events::BytesEnd<'_>) {
        let ln = e.local_name();
        let local = ln.as_ref();
        if local == "licenses" && self.in_licenses > 0 {
            self.in_licenses -= 1;
        }
        if local == "name" {
            self.capture_license_name = false;
        }
        apply_maven_end(
            local,
            &mut self.in_dependency,
            &mut self.current_group,
            &mut self.current_artifact,
            &mut self.current_version,
            &mut self.deps,
        );
    }
}

fn parse_maven_pom(content: &str) -> ExtractedManifest {
    use quick_xml::{events::Event, Reader};

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut state = PomParser::default();

    // One line per event kind: what each event *means* lives on `PomParser`, so
    // this loop stays a dispatch table rather than the machine itself.
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => state.start(e),
            Ok(Event::Text(ref e)) => state.text(e),
            Ok(Event::End(ref e)) => state.end(e),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let PomParser { deps, licenses, .. } = state;

    ExtractedManifest {
        dependencies: deps,
        // Maven has no README: the POM carries `<description>`, which is a
        // sentence rather than a document, and putting one where a reader
        // expects the other makes every package look thinly documented
        // (RFC 0007 §4.3).
        readme: None,
        // Several `<license>` entries mean the consumer may pick one, so they
        // join with the SPDX operator that says exactly that.
        license: if licenses.is_empty() {
            None
        } else {
            Some(licenses.join(" OR "))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sbom::extractor::readme::fixtures::zipped;

    const REAL_POM: &[u8] =
        br#"<project><licenses><license><name>GPL-3.0-only</name></license></licenses></project>"#;
    const DECOY_POM: &[u8] =
        br#"<project><licenses><license><name>MIT</name></license></licenses></project>"#;

    /// `ends_with("pom.xml")` matched a file not even named `pom.xml`, and took
    /// whichever came first in the zip.
    #[test]
    fn a_file_merely_ending_in_pom_xml_is_not_the_projects_pom() {
        let data = zipped(&[
            ("a/decoypom.xml", DECOY_POM),
            ("META-INF/maven/com.example/app/pom.xml", REAL_POM),
        ]);
        assert_eq!(
            extract_maven_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    /// A pom outside `META-INF/maven/{g}/{a}/` is not this jar's declaration,
    /// wherever it sits in the archive.
    #[test]
    fn a_pom_outside_the_meta_inf_location_is_not_the_projects_pom() {
        let data = zipped(&[
            ("a/pom.xml", DECOY_POM),
            ("META-INF/maven/com.example/app/pom.xml", REAL_POM),
        ]);
        assert_eq!(
            extract_maven_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    /// An uber/shaded jar carries one canonical pom per bundled dependency and
    /// nothing says which is its own. Reporting unknown is the honest answer;
    /// what this did before was report the first bundled dependency's licence as
    /// the artifact's.
    #[test]
    fn several_canonical_poms_are_ambiguous_not_first_wins() {
        let data = zipped(&[
            ("META-INF/maven/com.other/dep/pom.xml", DECOY_POM),
            ("META-INF/maven/com.example/app/pom.xml", REAL_POM),
        ]);
        assert_eq!(extract_maven_manifest(&data), ExtractedManifest::default());
    }

    fn pom(deps_xml: &str) -> String {
        format!(
            r#"<project>
                <groupId>com.example</groupId>
                <artifactId>app</artifactId>
                <version>9.9.9</version>
                <dependencies>{deps_xml}</dependencies>
            </project>"#
        )
    }

    #[test]
    fn parse_maven_pom_basic() {
        let xml = pom(r#"<dependency>
                <groupId>com.fasterxml.jackson.core</groupId>
                <artifactId>jackson-databind</artifactId>
                <version>2.15.0</version>
            </dependency>"#);
        let deps = parse_maven_pom(&xml).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "com.fasterxml.jackson.core:jackson-databind");
        assert_eq!(deps[0].version_req.as_deref(), Some("2.15.0"));
        assert_eq!(deps[0].ecosystem, "maven");
    }

    #[test]
    fn parse_maven_pom_no_group_id() {
        let xml = pom(r#"<dependency>
                <artifactId>standalone</artifactId>
                <version>1.0</version>
            </dependency>"#);
        let deps = parse_maven_pom(&xml).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "standalone");
    }

    #[test]
    fn parse_maven_pom_empty_artifact_id_skipped() {
        let xml = pom(r#"<dependency>
                <groupId>com.example</groupId>
                <version>1.0</version>
            </dependency>"#);
        let deps = parse_maven_pom(&xml).dependencies;
        assert!(deps.is_empty());
    }

    #[test]
    fn parse_maven_pom_multiple_dependencies() {
        let xml = pom(r#"<dependency>
                <groupId>g1</groupId>
                <artifactId>a1</artifactId>
                <version>1.0</version>
            </dependency>
            <dependency>
                <groupId>g2</groupId>
                <artifactId>a2</artifactId>
                <version>2.0</version>
            </dependency>"#);
        let deps = parse_maven_pom(&xml).dependencies;
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "g1:a1");
        assert_eq!(deps[0].version_req.as_deref(), Some("1.0"));
        assert_eq!(deps[1].name, "g2:a2");
        assert_eq!(deps[1].version_req.as_deref(), Some("2.0"));
    }

    #[test]
    fn parse_maven_pom_ignores_project_level_version() {
        // The project-level <version> (9.9.9, outside <dependencies>) must not
        // leak into the dependency's version.
        let xml = pom(r#"<dependency>
                <groupId>g1</groupId>
                <artifactId>a1</artifactId>
            </dependency>"#);
        let deps = parse_maven_pom(&xml).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].version_req, None);
    }

    /// `<name>` inside `<licenses>` is the licence; the project's own `<name>`
    /// (which `pom()` emits above the dependencies) is not.
    #[test]
    fn parse_maven_pom_license_from_licenses_block() {
        let xml = r#"<project>
                <name>My Application</name>
                <licenses>
                    <license><name>Apache License, Version 2.0</name><url>http://x</url></license>
                </licenses>
                <dependencies></dependencies>
            </project>"#;
        let manifest = parse_maven_pom(xml);
        assert_eq!(
            manifest.license.as_deref(),
            Some("Apache License, Version 2.0")
        );
    }

    #[test]
    fn parse_maven_pom_multiple_licenses_join_with_or() {
        let xml = r#"<project><licenses>
                <license><name>MIT</name></license>
                <license><name>EPL-2.0</name></license>
            </licenses></project>"#;
        assert_eq!(
            parse_maven_pom(xml).license.as_deref(),
            Some("MIT OR EPL-2.0")
        );
    }

    #[test]
    fn parse_maven_pom_without_licenses_is_none() {
        let xml = pom("");
        assert_eq!(parse_maven_pom(&xml).license, None);
    }

    #[test]
    fn extract_maven_deps_non_zip_returns_empty() {
        let data = Bytes::from_static(b"not a zip archive");
        assert_eq!(extract_maven_manifest(&data), ExtractedManifest::default());
    }

    #[test]
    fn extract_maven_deps_from_jar_with_pom() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let xml = pom(r#"<dependency>
                <groupId>g</groupId>
                <artifactId>a</artifactId>
                <version>1.2.3</version>
            </dependency>"#);

        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            writer
                .start_file(
                    "META-INF/maven/com.example/app/pom.xml",
                    SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(xml.as_bytes()).unwrap();
            writer.finish().unwrap();
        }

        let deps = extract_maven_manifest(&Bytes::from(buf)).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "g:a");
        assert_eq!(deps[0].version_req.as_deref(), Some("1.2.3"));
    }
}
