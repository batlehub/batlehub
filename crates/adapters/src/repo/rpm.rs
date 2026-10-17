//! RPM package parsing and YUM/DNF `repodata/` generation.
//!
//! We parse the RPM header with the `rpm` crate (built with `default-features =
//! false` so it does **not** pull the banned `rsa`/`pgp` stack) and emit the
//! classic `repodata/` set: `primary.xml`, `filelists.xml`, `other.xml`, and the
//! `repomd.xml` index that references them with their SHA-256 checksums.
//! `repomd.xml` is detached-signed (`repomd.xml.asc`) by [`super::openpgp`].

use sha2::{Digest, Sha256};

use batlehub_core::error::CoreError;

/// A provides/requires/conflicts/obsoletes entry.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RpmDep {
    pub name: String,
    pub flags: Option<String>,
    pub epoch: Option<String>,
    pub ver: Option<String>,
    pub rel: Option<String>,
}

/// A file entry from the RPM payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RpmFile {
    pub path: String,
    pub is_dir: bool,
}

/// Parsed RPM metadata, plus the package checksum and (once stored) its location.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RpmPackage {
    pub name: String,
    pub epoch: u32,
    pub version: String,
    pub release: String,
    pub arch: String,
    pub summary: String,
    pub description: String,
    pub license: String,
    pub url: String,
    pub vendor: String,
    pub group: String,
    pub build_host: String,
    pub source_rpm: String,
    pub build_time: u64,
    pub size_package: u64,
    pub size_installed: u64,
    pub size_archive: u64,
    pub provides: Vec<RpmDep>,
    pub requires: Vec<RpmDep>,
    pub files: Vec<RpmFile>,
    /// SHA-256 of the whole `.rpm` (the repodata `pkgid`).
    pub sha256: String,
    /// `repodata` location href, e.g. `packages/foo-1.0-1.x86_64.rpm`.
    pub location: String,
    /// Header byte range within the file (for `rpm:header-range`).
    pub header_start: u64,
    pub header_end: u64,
}

impl RpmPackage {
    pub fn nevra(&self) -> String {
        format!(
            "{}-{}-{}.{}",
            self.name, self.version, self.release, self.arch
        )
    }

    /// Storage-unique identifier for the package file and its sidecar.
    ///
    /// Standard RPM filenames omit the epoch, so two builds that share a
    /// name-version-release-arch but differ in `Epoch` would map to the same
    /// storage key and silently overwrite each other (and corrupt repodata).
    /// We disambiguate by prefixing the epoch when it is non-zero; the resulting
    /// href is still served verbatim from `<location>` in `primary.xml`, so DNF
    /// resolves it correctly.
    pub fn storage_id(&self) -> String {
        if self.epoch == 0 {
            self.nevra()
        } else {
            format!(
                "{}-{}-{}-{}.{}",
                self.epoch, self.name, self.version, self.release, self.arch
            )
        }
    }
}

/// The publish-time edge checks on the identifying header fields.
///
/// [`RpmPackage::storage_id`] is the stored file name and the sidecar key, and
/// it is one NEVRA only if it splits back from the right: an architecture has
/// no `.` or `-`, and rpmbuild already refuses a `-` in a version or release —
/// a hand-built header did not, and `(a, 1-2, 3)` was `(a-1, 2, 3)`'s file.
/// Each field is one path segment, and none carries a control character, which
/// XML 1.0 cannot represent: one such name made `primary.xml` unparseable for
/// every package in the repository.
///
/// What this cannot see is the epoch: `storage_id` prefixes a non-zero one, so
/// `(epoch 389, ds-base)` and `(epoch 0, 389-ds-base)` — a real Fedora name —
/// are one file. The handler refuses that against the stored sidecar.
pub fn check_coordinate(pkg: &RpmPackage) -> Result<(), CoreError> {
    use batlehub_core::services::validate_path_segment;
    for (kind, value) in [
        ("name", &pkg.name),
        ("version", &pkg.version),
        ("release", &pkg.release),
        ("arch", &pkg.arch),
    ] {
        validate_path_segment(kind, value)?;
        if value.chars().any(char::is_control) {
            return Err(CoreError::InvalidInput(format!(
                "rpm {kind} {value:?} contains a control character"
            )));
        }
    }
    for (kind, value, forbidden) in [
        ("version", &pkg.version, &['-'][..]),
        ("release", &pkg.release, &['-'][..]),
        ("arch", &pkg.arch, &['-', '.'][..]),
    ] {
        if value.contains(forbidden) {
            return Err(CoreError::InvalidInput(format!(
                "rpm {kind} '{value}' must not contain any of {forbidden:?}"
            )));
        }
    }
    Ok(())
}

/// Parse a `.rpm` into [`RpmPackage`]; `location` is the repo-relative href under
/// which the package will be served.
pub fn parse_rpm(bytes: &[u8], location: &str) -> Result<RpmPackage, CoreError> {
    let pkg = rpm::Package::parse(&mut std::io::Cursor::new(bytes))
        .map_err(|e| CoreError::InvalidInput(format!("invalid .rpm: {e}")))?;
    let md = &pkg.metadata;

    let get = |r: Result<&str, rpm::Error>| r.unwrap_or_default().to_string();

    let map_deps = |deps: Result<Vec<rpm::Dependency>, rpm::Error>| -> Vec<RpmDep> {
        deps.unwrap_or_default()
            .into_iter()
            .map(|d| {
                // Map the RPM dependency comparator to the repodata `flags` token
                // (`EQ`/`LT`/`GT`/`LE`/`GE`); `None` when the dependency is
                // unversioned, so DNF treats it as a plain (any-version) requirement.
                let flags = match d.flags.comparator_str() {
                    "=" => Some("EQ"),
                    "<" => Some("LT"),
                    ">" => Some("GT"),
                    "<=" => Some("LE"),
                    ">=" => Some("GE"),
                    _ => None,
                };
                RpmDep {
                    name: d.name,
                    flags: flags.map(str::to_owned),
                    epoch: None,
                    ver: if d.version.is_empty() {
                        None
                    } else {
                        Some(d.version)
                    },
                    rel: None,
                }
            })
            .collect()
    };

    let files = md
        .get_file_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|f| RpmFile {
            path: f.path().to_string_lossy().to_string(),
            is_dir: f.file_type() == rpm::FileType::Dir,
        })
        .collect();

    let offsets = md.get_package_segment_offsets();
    let (header_start, header_end) = (offsets.header, offsets.payload);

    Ok(RpmPackage {
        name: get(md.get_name()),
        epoch: md.get_epoch().unwrap_or(0),
        version: get(md.get_version()),
        release: get(md.get_release()),
        arch: get(md.get_arch()),
        summary: get(md.get_summary()),
        description: get(md.get_description()),
        license: get(md.get_license()),
        url: get(md.get_url()),
        vendor: get(md.get_vendor()),
        group: md
            .get_group()
            .map(|g| g.to_string())
            .unwrap_or_else(|_| "Unspecified".to_string()),
        build_host: md.get_build_host().unwrap_or_default().to_string(),
        source_rpm: get(md.get_source_rpm()),
        build_time: md.get_build_time().unwrap_or(0),
        size_package: bytes.len() as u64,
        size_installed: md.get_installed_size().unwrap_or(0),
        // The `rpm` crate does not expose the (compressed) archive size; the
        // installed size is the field DNF actually uses for solving.
        size_archive: 0,
        provides: map_deps(md.get_provides()),
        requires: map_deps(md.get_requires()),
        files,
        sha256: hex::encode(Sha256::digest(bytes)),
        location: location.to_string(),
        header_start,
        header_end,
    })
}

// ── XML generation ──────────────────────────────────────────────────────────

/// Escape `s` for an XML 1.0 text node or double-quoted attribute.
///
/// Two things beyond the five entities, both found by `fuzz_repo_index_render`:
///
/// - **Characters XML 1.0 cannot carry at all** — C0 controls other than tab,
///   newline and carriage return, and U+FFFE/U+FFFF — become U+FFFD. Not even a
///   character reference may name them, so one `\x01` in one package's summary
///   made `primary.xml` unparseable and DNF refused the whole repository. The
///   identifying fields are refused at publish (`check_coordinate`); this is
///   the free text, and every sidecar stored before that check existed.
/// - **Tab, newline and carriage return** become character references, because
///   a parser normalises them literally — `\r\n` to `\n` in text, all three to
///   a space in an attribute — and the value read back would not be the value
///   written.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\u{0}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

fn version_attrs(p: &RpmPackage) -> String {
    format!(
        r#"epoch="{}" ver="{}" rel="{}""#,
        p.epoch,
        esc(&p.version),
        esc(&p.release)
    )
}

fn dep_entries(deps: &[RpmDep], indent: &str) -> String {
    let mut out = String::new();
    for d in deps {
        out.push_str(indent);
        out.push_str(&format!(r#"<rpm:entry name="{}""#, esc(&d.name)));
        if let Some(v) = &d.ver {
            // Preserve the real comparator; default to `EQ` only when a version is
            // present without a parsed comparator (RPM auto-provides are exact).
            let flags = d.flags.as_deref().unwrap_or("EQ");
            out.push_str(&format!(r#" flags="{}" ver="{}""#, flags, esc(v)));
        }
        out.push_str("/>\n");
        let _ = &d.epoch;
        let _ = &d.rel;
    }
    out
}

/// Generate `primary.xml` for the given packages.
pub fn primary_xml(packages: &[RpmPackage]) -> String {
    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(&format!(
        r#"<metadata xmlns="http://linux.duke.edu/metadata/common" xmlns:rpm="http://linux.duke.edu/metadata/rpm" packages="{}">"#,
        packages.len()
    ));
    out.push('\n');
    for p in packages {
        out.push_str("<package type=\"rpm\">\n");
        out.push_str(&format!("  <name>{}</name>\n", esc(&p.name)));
        out.push_str(&format!("  <arch>{}</arch>\n", esc(&p.arch)));
        out.push_str(&format!("  <version {}/>\n", version_attrs(p)));
        out.push_str(&format!(
            "  <checksum type=\"sha256\" pkgid=\"YES\">{}</checksum>\n",
            p.sha256
        ));
        out.push_str(&format!("  <summary>{}</summary>\n", esc(&p.summary)));
        out.push_str(&format!(
            "  <description>{}</description>\n",
            esc(&p.description)
        ));
        out.push_str(&format!("  <packager>{}</packager>\n", esc(&p.build_host)));
        out.push_str(&format!("  <url>{}</url>\n", esc(&p.url)));
        out.push_str(&format!(
            "  <time file=\"{}\" build=\"{}\"/>\n",
            p.build_time, p.build_time
        ));
        out.push_str(&format!(
            "  <size package=\"{}\" installed=\"{}\" archive=\"{}\"/>\n",
            p.size_package, p.size_installed, p.size_archive
        ));
        out.push_str(&format!("  <location href=\"{}\"/>\n", esc(&p.location)));
        out.push_str("  <format>\n");
        out.push_str(&format!(
            "    <rpm:license>{}</rpm:license>\n",
            esc(&p.license)
        ));
        out.push_str(&format!(
            "    <rpm:vendor>{}</rpm:vendor>\n",
            esc(&p.vendor)
        ));
        out.push_str(&format!("    <rpm:group>{}</rpm:group>\n", esc(&p.group)));
        out.push_str(&format!(
            "    <rpm:buildhost>{}</rpm:buildhost>\n",
            esc(&p.build_host)
        ));
        out.push_str(&format!(
            "    <rpm:sourcerpm>{}</rpm:sourcerpm>\n",
            esc(&p.source_rpm)
        ));
        out.push_str(&format!(
            "    <rpm:header-range start=\"{}\" end=\"{}\"/>\n",
            p.header_start, p.header_end
        ));
        if !p.provides.is_empty() {
            out.push_str("    <rpm:provides>\n");
            out.push_str(&dep_entries(&p.provides, "      "));
            out.push_str("    </rpm:provides>\n");
        }
        if !p.requires.is_empty() {
            out.push_str("    <rpm:requires>\n");
            out.push_str(&dep_entries(&p.requires, "      "));
            out.push_str("    </rpm:requires>\n");
        }
        out.push_str("  </format>\n");
        out.push_str("</package>\n");
    }
    out.push_str("</metadata>\n");
    out
}

/// Generate `filelists.xml`.
pub fn filelists_xml(packages: &[RpmPackage]) -> String {
    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(&format!(
        r#"<filelists xmlns="http://linux.duke.edu/metadata/filelists" packages="{}">"#,
        packages.len()
    ));
    out.push('\n');
    for p in packages {
        out.push_str(&format!(
            "<package pkgid=\"{}\" name=\"{}\" arch=\"{}\">\n",
            p.sha256,
            esc(&p.name),
            esc(&p.arch)
        ));
        out.push_str(&format!("  <version {}/>\n", version_attrs(p)));
        for f in &p.files {
            if f.is_dir {
                out.push_str(&format!("  <file type=\"dir\">{}</file>\n", esc(&f.path)));
            } else {
                out.push_str(&format!("  <file>{}</file>\n", esc(&f.path)));
            }
        }
        out.push_str("</package>\n");
    }
    out.push_str("</filelists>\n");
    out
}

/// Generate `other.xml` (changelogs omitted — we do not parse them).
pub fn other_xml(packages: &[RpmPackage]) -> String {
    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(&format!(
        r#"<otherdata xmlns="http://linux.duke.edu/metadata/other" packages="{}">"#,
        packages.len()
    ));
    out.push('\n');
    for p in packages {
        out.push_str(&format!(
            "<package pkgid=\"{}\" name=\"{}\" arch=\"{}\">\n",
            p.sha256,
            esc(&p.name),
            esc(&p.arch)
        ));
        out.push_str(&format!("  <version {}/>\n", version_attrs(p)));
        out.push_str("</package>\n");
    }
    out.push_str("</otherdata>\n");
    out
}

/// One `<data>` entry in `repomd.xml`.
pub struct RepoMdData {
    /// `primary` / `filelists` / `other`.
    pub kind: String,
    /// `repodata`-relative href of the gzipped file.
    pub href: String,
    /// SHA-256 of the gzipped file.
    pub checksum: String,
    /// SHA-256 of the uncompressed file.
    pub open_checksum: String,
    pub size: u64,
    pub open_size: u64,
    pub timestamp: u64,
}

impl RepoMdData {
    pub fn new(kind: &str, href: &str, gz: &[u8], plain: &[u8], timestamp: u64) -> Self {
        Self {
            kind: kind.to_string(),
            href: href.to_string(),
            checksum: hex::encode(Sha256::digest(gz)),
            open_checksum: hex::encode(Sha256::digest(plain)),
            size: gz.len() as u64,
            open_size: plain.len() as u64,
            timestamp,
        }
    }
}

/// Generate `repomd.xml` referencing the repodata files.
pub fn repomd_xml(entries: &[RepoMdData]) -> String {
    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(
        r#"<repomd xmlns="http://linux.duke.edu/metadata/repo" xmlns:rpm="http://linux.duke.edu/metadata/rpm">"#,
    );
    out.push('\n');
    for e in entries {
        out.push_str(&format!("  <data type=\"{}\">\n", e.kind));
        out.push_str(&format!(
            "    <checksum type=\"sha256\">{}</checksum>\n",
            e.checksum
        ));
        out.push_str(&format!(
            "    <open-checksum type=\"sha256\">{}</open-checksum>\n",
            e.open_checksum
        ));
        out.push_str(&format!("    <location href=\"{}\"/>\n", esc(&e.href)));
        out.push_str(&format!("    <timestamp>{}</timestamp>\n", e.timestamp));
        out.push_str(&format!("    <size>{}</size>\n", e.size));
        out.push_str(&format!("    <open-size>{}</open-size>\n", e.open_size));
        out.push_str("  </data>\n");
    }
    out.push_str("</repomd>\n");
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn esc_keeps_the_document_well_formed_and_the_value_readable() {
        assert_eq!(super::esc("a\u{1}b"), "a\u{fffd}b");
        assert_eq!(
            super::esc("x\r\ny\t\"<&>"),
            "x&#13;&#10;y&#9;&quot;&lt;&amp;&gt;"
        );
        assert_eq!(super::esc("\u{ffff}"), "\u{fffd}");
    }

    #[test]
    fn check_coordinate_refuses_what_storage_id_cannot_split_back() {
        let ok = super::RpmPackage {
            name: "389-ds-base".into(),
            version: "2.4".into(),
            release: "1.fc40".into(),
            arch: "x86_64".into(),
            ..Default::default()
        };
        assert!(super::check_coordinate(&ok).is_ok());
        for (field, value) in [
            ("name", "a\u{1}"),
            ("name", "a/b"),
            ("version", "1-2"),
            ("release", "1-2"),
            ("arch", "noarch.x"),
            ("arch", "x86-64"),
        ] {
            let mut p = ok.clone();
            match field {
                "name" => p.name = value.into(),
                "version" => p.version = value.into(),
                "release" => p.release = value.into(),
                _ => p.arch = value.into(),
            }
            assert!(super::check_coordinate(&p).is_err(), "{field}={value:?}");
        }
    }

    use super::*;

    fn sample() -> RpmPackage {
        RpmPackage {
            name: "hello".into(),
            epoch: 0,
            version: "1.0".into(),
            release: "1".into(),
            arch: "x86_64".into(),
            summary: "Hi & bye".into(),
            description: "A <test> package".into(),
            license: "MIT".into(),
            provides: vec![RpmDep {
                name: "hello".into(),
                ver: Some("1.0".into()),
                ..Default::default()
            }],
            requires: vec![RpmDep {
                name: "libc.so.6".into(),
                ..Default::default()
            }],
            files: vec![
                RpmFile {
                    path: "/usr/bin/hello".into(),
                    is_dir: false,
                },
                RpmFile {
                    path: "/usr/share/hello".into(),
                    is_dir: true,
                },
            ],
            sha256: "abc123".into(),
            location: "packages/hello-1.0-1.x86_64.rpm".into(),
            size_package: 100,
            header_end: 100,
            ..Default::default()
        }
    }

    /// Build a real (unsigned) `.rpm` in-memory for parse tests.
    fn build_rpm() -> Vec<u8> {
        let pkg = rpm::PackageBuilder::new("hello", "1.0", "MIT", "x86_64", "a greeting")
            .with_file_contents(
                b"#!/bin/sh\necho hi\n".to_vec(),
                rpm::FileOptions::new("/usr/bin/hello").mode(0o100755),
            )
            .unwrap()
            .build()
            .unwrap();
        let mut buf = Vec::new();
        pkg.write(&mut buf).unwrap();
        buf
    }

    #[test]
    fn parse_rpm_reads_nevra_files_and_checksum() {
        let bytes = build_rpm();
        let pkg = parse_rpm(&bytes, "packages/hello-1.0-1.x86_64.rpm").unwrap();
        assert_eq!(pkg.name, "hello");
        assert_eq!(pkg.version, "1.0");
        assert_eq!(pkg.arch, "x86_64");
        assert_eq!(pkg.license, "MIT");
        assert_eq!(pkg.sha256, hex::encode(Sha256::digest(&bytes)));
        assert_eq!(pkg.location, "packages/hello-1.0-1.x86_64.rpm");
        assert!(pkg.header_end >= pkg.header_start);
        // The file we added shows up in the payload listing.
        assert!(pkg.files.iter().any(|f| f.path == "/usr/bin/hello"));
        // RPM auto-provides the package name.
        assert!(pkg.provides.iter().any(|d| d.name == "hello"));

        // And the parsed package flows through repodata generation.
        let xml = primary_xml(&[pkg]);
        assert!(xml.contains("<name>hello</name>"));
        assert!(xml.contains("packages/hello-1.0-1.x86_64.rpm"));
    }

    #[test]
    fn primary_xml_escapes_and_includes_core_fields() {
        let xml = primary_xml(&[sample()]);
        assert!(xml.contains(r#"packages="1""#));
        assert!(xml.contains("<name>hello</name>"));
        assert!(xml.contains(r#"<version epoch="0" ver="1.0" rel="1"/>"#));
        assert!(xml.contains(r#"pkgid="YES">abc123</checksum>"#));
        assert!(xml.contains("Hi &amp; bye"));
        assert!(xml.contains("A &lt;test&gt; package"));
        assert!(xml.contains(r#"<location href="packages/hello-1.0-1.x86_64.rpm"/>"#));
        assert!(xml.contains(r#"<rpm:entry name="hello" flags="EQ" ver="1.0"/>"#));
    }

    #[test]
    fn filelists_marks_directories() {
        let xml = filelists_xml(&[sample()]);
        assert!(xml.contains("<file>/usr/bin/hello</file>"));
        assert!(xml.contains(r#"<file type="dir">/usr/share/hello</file>"#));
    }

    #[test]
    fn repomd_references_entries_with_checksums() {
        let plain = b"<primary/>";
        let gz = b"gzipped";
        let data = RepoMdData::new("primary", "repodata/primary.xml.gz", gz, plain, 42);
        let xml = repomd_xml(&[data]);
        assert!(xml.contains(r#"<data type="primary">"#));
        assert!(xml.contains(&hex::encode(Sha256::digest(gz))));
        assert!(xml.contains(&hex::encode(Sha256::digest(plain))));
        assert!(xml.contains("repodata/primary.xml.gz"));
        assert!(xml.contains("<timestamp>42</timestamp>"));
    }

    #[test]
    fn nevra_format() {
        assert_eq!(sample().nevra(), "hello-1.0-1.x86_64");
    }

    #[test]
    fn storage_id_includes_epoch_only_when_nonzero() {
        let mut p = sample();
        assert_eq!(p.storage_id(), "hello-1.0-1.x86_64");
        p.epoch = 2;
        // Epoch-distinct builds get a distinct storage id (no overwrite).
        assert_eq!(p.storage_id(), "2-hello-1.0-1.x86_64");
        assert_ne!(p.storage_id(), sample().storage_id());
    }

    #[test]
    fn dep_entries_preserve_real_comparator() {
        let deps = vec![
            RpmDep {
                name: "libc".into(),
                flags: Some("GE".into()),
                ver: Some("2.34".into()),
                ..Default::default()
            },
            RpmDep {
                name: "old".into(),
                flags: Some("LT".into()),
                ver: Some("3.0".into()),
                ..Default::default()
            },
        ];
        let out = dep_entries(&deps, "");
        assert!(out.contains(r#"<rpm:entry name="libc" flags="GE" ver="2.34"/>"#));
        assert!(out.contains(r#"<rpm:entry name="old" flags="LT" ver="3.0"/>"#));
    }
}
