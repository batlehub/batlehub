use batlehub_core::ports::{ExtractedManifest, ExtractedReadme, SbomDependency};
use bytes::Bytes;

use super::{anchor, readme};
use batlehub_core::services::readme::detect;

pub(super) fn extract_pypi_manifest(data: &Bytes) -> ExtractedManifest {
    // Try wheel (zip) first, then sdist (tar.gz).
    extract_pypi_wheel(data)
        .or_else(|| extract_pypi_sdist(data))
        .unwrap_or_default()
}

fn extract_pypi_wheel(data: &Bytes) -> Option<ExtractedManifest> {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let cursor = Cursor::new(data.as_ref());
    let Ok(mut archive) = ZipArchive::new(cursor) else {
        tracing::warn!("sbom: failed to parse pypi wheel manifest, treating as no dependencies");
        return None;
    };

    // The single `{dist}-{version}.dist-info/METADATA` PEP 427 allows a wheel.
    // `ends_with(".dist-info/METADATA")` matched one at any depth and took the
    // first, so a planted `evil.dist-info/METADATA` decided the answer. See
    // `anchor`.
    let matches: Vec<usize> = (0..archive.len())
        .filter(|i| {
            archive
                .by_index(*i)
                .is_ok_and(|f| f.name().is_ok_and(|n| anchor::is_wheel_metadata(&n)))
        })
        .collect();
    let index = anchor::sole(matches)?;

    let mut file = match archive.by_index(index) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(error = %e, "sbom: failed to read pypi wheel manifest, treating as no dependencies");
            return None;
        }
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        tracing::warn!("sbom: failed to parse pypi wheel manifest, treating as no dependencies");
        return None;
    }
    Some(parse_pep_metadata(&content))
}

fn extract_pypi_sdist(data: &Bytes) -> Option<ExtractedManifest> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let gz = GzDecoder::new(data.as_ref());
    let mut archive = Archive::new(gz);

    let Ok(entries) = archive.entries() else {
        tracing::warn!("sbom: failed to parse pypi sdist manifest, treating as no dependencies");
        return None;
    };

    // `{name}-{version}/PKG-INFO` at the wrapper directory's depth, with
    // `METADATA` as the fallback spelling. A bare `PKG-INFO`/`METADATA`
    // filename matched at *any* depth, so a vendored — or planted — copy won on
    // tar order. See `anchor`.
    let mut pkg_info: Vec<String> = Vec::new();
    let mut alias: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(path) = entry.path() else { continue };
        let path = path.to_string_lossy().into_owned();
        let target = if anchor::is_sdist_pkg_info(&path) {
            &mut pkg_info
        } else if anchor::is_sdist_metadata_alias(&path) {
            &mut alias
        } else {
            continue;
        };
        let mut reader = entry;
        let mut content = String::new();
        if reader.read_to_string(&mut content).is_ok() {
            target.push(content);
        }
    }
    anchor::sole(pkg_info)
        .or_else(|| anchor::sole(alias))
        .map(|content| parse_pep_metadata(&content))
}

fn parse_pep_metadata(content: &str) -> ExtractedManifest {
    let dependencies = content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("Requires-Dist:").map(|rest| {
                // "Requires-Dist: requests >=2.0" or "requests"
                let dep = rest.trim().split(';').next().unwrap_or(rest.trim());
                let mut parts = dep.splitn(2, ' ');
                let name = parts.next().unwrap_or("").trim().to_owned();
                let ver = parts.next().map(|v| v.trim().to_owned());
                SbomDependency {
                    name,
                    version_req: ver.filter(|v| !v.is_empty()),
                    ecosystem: "pypi".into(),
                }
            })
        })
        .filter(|d| !d.name.is_empty())
        .collect();

    ExtractedManifest {
        dependencies,
        license: parse_pep_license(content),
        readme: parse_pep_description(content),
    }
}

/// The long description a PEP 566 metadata file carries, and its declared
/// markup.
///
/// Two shapes, both current:
///
/// - the **body**, everything after the blank line that ends the headers. This
///   is what every modern build backend writes, and it is why a wheel's
///   `METADATA` is often mostly prose.
/// - the `Description:` **header**, with continuation lines indented by eight
///   spaces. Older setuptools wrote this, and plenty of published sdists still
///   carry it.
///
/// `Description-Content-Type` names the markup in both cases; PEP 566 says an
/// absent one means plain text, and it is not guessed into a renderer.
fn parse_pep_description(content: &str) -> Option<ExtractedReadme> {
    // Split first, and read the header **only out of the headers**. Scanning the
    // whole file let the long description choose its own renderer: a README
    // containing a line `Description-Content-Type: text/html` — entirely
    // ordinary in a packaging tool's own README — was read as the declaration,
    // and prose that should have been escaped as `Plain` went down the HTML
    // path. An upstream must not be able to pick which renderer runs.
    //
    // `\r\n\r\n` as well as `\n\n`: a CRLF-encoded `METADATA`/`PKG-INFO` matched
    // neither the split nor, therefore, the body — it fell through to the legacy
    // `Description:` branch, found nothing, and reported no README at all.
    let split = content
        .split_once("\r\n\r\n")
        .or_else(|| content.split_once("\n\n"));
    let headers = split.map_or(content, |(headers, _)| headers);

    let format = detect::format_from_content_type(
        headers
            .lines()
            .find_map(|l| l.trim().strip_prefix("Description-Content-Type:"))
            .map(str::trim),
    );

    // The body wins: when a file has both, the header is the legacy copy.
    let body = split
        .map(|(_, body)| body)
        .filter(|body| !body.trim().is_empty());
    if let Some(body) = body {
        return readme::from_manifest_field(body, "METADATA", format);
    }

    let mut description = String::new();
    let mut in_description = false;
    for line in content.lines() {
        if let Some(first) = line.strip_prefix("Description:") {
            in_description = true;
            description.push_str(first.trim_start());
            continue;
        }
        if !in_description {
            continue;
        }
        // A continuation line is indented; anything else ends the field.
        match line.strip_prefix("        ") {
            Some(rest) => {
                description.push('\n');
                description.push_str(rest);
            }
            None if line.trim().is_empty() => description.push('\n'),
            None => break,
        }
    }
    readme::from_manifest_field(&description, "METADATA", format)
}

/// setuptools wrote `UNKNOWN` into `License:` (and `Summary:`, `Home-page:`, …)
/// by default for years, so a large share of older wheels and sdists carry it.
///
/// It has to read back as *unknown*, not as a licence. `LicenseGateRule` treats
/// any recorded string as a known declaration, so storing `"UNKNOWN"` takes the
/// package out of the `allow_unknown` path and judges it against the allow list,
/// which `UNKNOWN` never matches: an operator with
/// `allow = ["MIT"], allow_unknown = true` would find `pip install` of an older
/// MIT-licensed sdist denied — precisely the outcome `allow_unknown` exists to
/// prevent.
fn is_placeholder_license(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_uppercase().as_str(),
        "UNKNOWN" | "NONE" | "N/A" | "NOASSERTION"
    )
}

/// PEP 639's `License-Expression` first, then the legacy `License`, then the
/// `License ::` trove classifier.
///
/// The order is the specification's own precedence, and it matters: a package
/// mid-migration carries both, with `License` holding free text like
/// `"BSD-3-Clause or later, see LICENSE"` while `License-Expression` holds the
/// SPDX id. Reading the legacy field first would hand the gate prose.
///
/// The classifier fallback keeps only the trailing segment — `License :: OSI
/// Approved :: MIT License` is a taxonomy path, not an expression, so the
/// stored value is `MIT License`, which the gate's normalisation handles.
fn parse_pep_license(content: &str) -> Option<String> {
    let field = |name: &str| {
        content.lines().find_map(|line| {
            line.trim()
                .strip_prefix(name)
                .map(str::trim)
                .filter(|v| !v.is_empty() && !is_placeholder_license(v))
                .map(str::to_owned)
        })
    };

    if let Some(v) = field("License-Expression:") {
        return Some(v);
    }
    if let Some(v) = field("License:") {
        // A multi-line `License:` continuation is the licence *text* inlined,
        // which is not an expression. One line is all this reads.
        return Some(v);
    }

    let classifiers: Vec<String> = content
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Classifier:"))
        .map(str::trim)
        .filter(|c| c.starts_with("License ::"))
        .filter_map(|c| c.rsplit("::").next())
        .map(str::trim)
        .filter(|c| !c.is_empty() && *c != "OSI Approved")
        .map(str::to_owned)
        .collect();

    if classifiers.is_empty() {
        None
    } else {
        Some(classifiers.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    /// The long description must not be able to choose its own renderer. A
    /// README that *discusses* `Description-Content-Type` was being read as
    /// declaring it, and HTML-escaped prose went down the HTML path instead.
    #[test]
    fn a_content_type_in_the_body_is_not_a_header() {
        let meta = "Metadata-Version: 2.1\nName: demo\nVersion: 1.0\n\n                    Set this in your pyproject:\n\nDescription-Content-Type: text/html\n";
        let found = parse_pep_description(meta).expect("a README");
        assert_eq!(found.format, batlehub_core::entities::ReadmeFormat::Plain);
    }

    /// A CRLF-encoded `METADATA` has a body too. It matched neither split, fell
    /// through to the legacy `Description:` branch, and reported no README.
    #[test]
    fn a_crlf_metadata_still_has_a_body() {
        let meta = "Metadata-Version: 2.1\r\nName: demo\r\n                    Description-Content-Type: text/markdown\r\n\r\n# Demo\r\n";
        let found = parse_pep_description(meta).expect("a README");
        assert_eq!(
            found.format,
            batlehub_core::entities::ReadmeFormat::Markdown
        );
        assert!(found.content.contains("# Demo"), "{:?}", found.content);
    }

    use super::*;

    const REAL_META: &[u8] = b"Name: demo\nLicense-Expression: GPL-3.0-only\n";
    const DECOY_META: &[u8] = b"Name: decoy\nLicense-Expression: MIT\n";

    /// A wheel has exactly one `.dist-info`. `ends_with(".dist-info/METADATA")`
    /// accepted one nested anywhere and zip order made it win.
    #[test]
    fn a_nested_dist_info_is_not_the_wheels_own() {
        let data = zipped(&[
            ("vendor/decoy.dist-info/METADATA", DECOY_META),
            ("demo-1.0.0.dist-info/METADATA", REAL_META),
        ]);
        assert_eq!(
            extract_pypi_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    /// Two at the wheel's own depth is malformed, so the answer is unknown
    /// rather than whichever was written first.
    #[test]
    fn two_dist_info_directories_are_ambiguous_not_first_wins() {
        let data = zipped(&[
            ("aaa.dist-info/METADATA", DECOY_META),
            ("demo-1.0.0.dist-info/METADATA", REAL_META),
        ]);
        assert_eq!(extract_pypi_manifest(&data), ExtractedManifest::default());
    }

    /// The sdist half of the same defect: a bare `PKG-INFO` filename matched at
    /// any depth, so a vendored copy won on tar order.
    #[test]
    fn a_nested_pkg_info_is_not_the_sdists_own() {
        let data = targz(&[
            ("demo-1.0.0/vendor/dep/PKG-INFO", DECOY_META),
            ("demo-1.0.0/PKG-INFO", REAL_META),
        ]);
        assert_eq!(
            extract_pypi_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    /// `PKG-INFO` is unambiguously the one to read, so an sdist carrying the
    /// `METADATA` spelling beside it is not ambiguous — the fallback only
    /// applies when there is no `PKG-INFO`.
    #[test]
    fn pkg_info_wins_over_the_metadata_spelling_beside_it() {
        let data = targz(&[
            ("demo-1.0.0/METADATA", DECOY_META),
            ("demo-1.0.0/PKG-INFO", REAL_META),
        ]);
        assert_eq!(
            extract_pypi_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    #[test]
    fn parse_pep_metadata_basic() {
        let metadata =
            "Name: requests\nVersion: 2.31.0\nRequires-Dist: urllib3 >=1.21\nRequires-Dist: certifi\n";
        let deps = parse_pep_metadata(metadata).dependencies;
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "urllib3");
        assert_eq!(deps[0].version_req.as_deref(), Some(">=1.21"));
        assert_eq!(deps[1].name, "certifi");
        assert!(deps[1].version_req.is_none());
    }

    #[test]
    fn parse_pep_metadata_strips_environment_markers() {
        let deps = parse_pep_metadata("Requires-Dist: pytest >=7 ; extra == 'test'\n").dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "pytest");
        assert_eq!(deps[0].version_req.as_deref(), Some(">=7"));
        assert_eq!(deps[0].ecosystem, "pypi");
    }

    #[test]
    fn parse_pep_license_prefers_expression_over_legacy_field() {
        let metadata =
            "License: BSD-3-Clause or later, see LICENSE\nLicense-Expression: BSD-3-Clause\n";
        assert_eq!(parse_pep_license(metadata).as_deref(), Some("BSD-3-Clause"));
    }

    #[test]
    fn parse_pep_license_falls_back_to_legacy_field() {
        assert_eq!(parse_pep_license("License: MIT\n").as_deref(), Some("MIT"));
    }

    /// `License :: OSI Approved :: MIT License` is a taxonomy path; only the
    /// leaf is a licence name, and the `OSI Approved` interior node is not one.
    #[test]
    fn parse_pep_license_falls_back_to_classifier_leaf() {
        let metadata = "Classifier: Programming Language :: Python\nClassifier: License :: OSI Approved :: MIT License\n";
        assert_eq!(parse_pep_license(metadata).as_deref(), Some("MIT License"));
    }

    #[test]
    fn parse_pep_license_ignores_license_file_field() {
        // `License-File` names a file, like Cargo's `license-file`.
        assert_eq!(parse_pep_license("License-File: LICENSE\n"), None);
    }

    #[test]
    fn parse_pep_license_absent_is_none() {
        assert_eq!(parse_pep_license("Name: x\nVersion: 1.0\n"), None);
    }

    /// setuptools' default placeholder must read back as unknown, or the gate
    /// judges it against the allow list and denies an otherwise fine package.
    #[test]
    fn parse_pep_license_placeholder_is_none() {
        assert_eq!(parse_pep_license("License: UNKNOWN\n"), None);
        assert_eq!(parse_pep_license("License: unknown\n"), None);
        assert_eq!(parse_pep_license("License-Expression: UNKNOWN\n"), None);
    }

    /// …but it must not shadow a real declaration further down.
    #[test]
    fn parse_pep_license_placeholder_falls_through_to_the_classifier() {
        let metadata = "License: UNKNOWN\nClassifier: License :: OSI Approved :: MIT License\n";
        assert_eq!(parse_pep_license(metadata).as_deref(), Some("MIT License"));
    }

    #[test]
    fn extract_pypi_deps_reads_wheel_metadata() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file(
                "requests-2.31.0.dist-info/METADATA",
                SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(b"Requires-Dist: urllib3 >=1.21\n").unwrap();
            zw.finish().unwrap();
        }
        let deps = extract_pypi_manifest(&Bytes::from(buf)).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "urllib3");
        assert_eq!(deps[0].ecosystem, "pypi");
    }

    #[test]
    fn extract_pypi_deps_reads_sdist_pkg_info() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let pkg_info: &[u8] = b"Requires-Dist: certifi\n";
        let mut tar_buf = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_buf);
            let mut h = tar::Header::new_gnu();
            h.set_size(pkg_info.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "requests-2.31.0/PKG-INFO", pkg_info)
                .unwrap();
            b.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_buf).unwrap();
        let deps = extract_pypi_manifest(&Bytes::from(gz.finish().unwrap())).dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "certifi");
    }

    #[test]
    fn extract_pypi_deps_on_garbage_is_empty() {
        assert_eq!(
            extract_pypi_manifest(&Bytes::from_static(b"not an archive")),
            ExtractedManifest::default()
        );
    }

    use super::super::readme::fixtures::{targz, zipped};
    use batlehub_core::entities::ReadmeFormat;

    /// The modern shape: the long description is the METADATA **body**, after
    /// the blank line that ends the headers, with its markup declared by
    /// `Description-Content-Type`.
    #[test]
    fn a_wheels_metadata_body_is_the_readme() {
        let data = zipped(&[(
            "x-1.0.dist-info/METADATA",
            b"Metadata-Version: 2.1\nName: x\nLicense-Expression: MIT\n\
              Description-Content-Type: text/markdown\nRequires-Dist: requests\n\
              \n# x\n\nDoes a thing.\n"
                .as_slice(),
        )]);
        let manifest = extract_pypi_manifest(&data);
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.dependencies.len(), 1);
        let readme = manifest.readme.expect("description read");
        assert_eq!(readme.content, "# x\n\nDoes a thing.");
        assert_eq!(readme.format, ReadmeFormat::Markdown);
        assert_eq!(readme.path, "METADATA");
    }

    /// The legacy shape older setuptools wrote: a `Description:` header whose
    /// continuation lines are indented by eight spaces. Plenty of published
    /// sdists still carry it.
    #[test]
    fn the_legacy_description_header_is_read_with_its_continuations() {
        let data = targz(&[(
            "x-1.0/PKG-INFO",
            b"Metadata-Version: 1.2\nName: x\nDescription: First line\n\
              \x20       Second line\n\x20       Third line\nPlatform: UNKNOWN\n"
                .as_slice(),
        )]);
        let readme = extract_pypi_manifest(&data)
            .readme
            .expect("description read");
        assert_eq!(readme.content, "First line\nSecond line\nThird line");
        // Nothing declared the markup, and PEP 566 says that means plain text.
        assert_eq!(readme.format, ReadmeFormat::Plain);
    }

    /// An RST description is stored as RST and shown as escaped source: docutils
    /// is the only faithful renderer, and a partial one renders some documents
    /// subtly wrong (RFC 0007 §3).
    #[test]
    fn a_declared_rst_description_stays_rst() {
        let data = zipped(&[(
            "x-1.0.dist-info/METADATA",
            b"Name: x\nDescription-Content-Type: text/x-rst\n\nHeading\n=======\n".as_slice(),
        )]);
        assert_eq!(
            extract_pypi_manifest(&data).readme.unwrap().format,
            ReadmeFormat::Rst
        );
    }

    /// Headers and nothing else is a package with no long description.
    #[test]
    fn metadata_with_no_description_reports_none() {
        let data = zipped(&[(
            "x-1.0.dist-info/METADATA",
            b"Metadata-Version: 2.1\nName: x\nLicense-Expression: MIT\n".as_slice(),
        )]);
        let manifest = extract_pypi_manifest(&data);
        assert!(manifest.readme.is_none());
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
    }
}
