use batlehub_core::ports::{ExtractedManifest, SbomDependency};
use bytes::Bytes;

use super::{anchor, readme};

pub(super) fn extract_npm_manifest(data: &Bytes) -> ExtractedManifest {
    use std::io::Read;

    let mut archive = readme::open_targz(data);

    let Ok(entries) = archive.entries() else {
        tracing::warn!("sbom: failed to parse npm manifest, treating as no dependencies");
        return ExtractedManifest::default();
    };

    // The top-level `package/package.json`. The depth check was already here —
    // it was the only extractor that constrained position at all — but taking
    // the *first* entry at that depth still let `aaa/package.json`, placed ahead
    // of it in the tarball, win on order. See `anchor`.
    let mut candidates: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(path) = entry.path() else { continue };
        if !anchor::is_npm_manifest(&path.to_string_lossy()) {
            continue;
        }
        let mut reader = entry;
        let mut content = String::new();
        if reader.read_to_string(&mut content).is_err() {
            tracing::warn!("sbom: failed to parse npm manifest, treating as no dependencies");
            return ExtractedManifest::default();
        }
        candidates.push(content);
    }

    let Some(content) = anchor::sole(candidates) else {
        return ExtractedManifest::default();
    };
    let mut manifest = parse_npm_package_json(&content);
    // The tarball's own README, for the versions whose packument carries none —
    // which is why npm's `readme_support()` is `MetadataThenArchive`. Every path
    // is prefixed `package/`.
    manifest.readme = readme::readme_from_targz(data, readme::root_readme_matcher(1));
    manifest
}

fn parse_npm_package_json(content: &str) -> ExtractedManifest {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(content) else {
        tracing::warn!("sbom: failed to parse npm manifest, treating as no dependencies");
        return ExtractedManifest::default();
    };

    let mut deps = Vec::new();
    for key in &["dependencies", "peerDependencies"] {
        if let Some(obj) = val.get(key).and_then(|v| v.as_object()) {
            for (name, ver) in obj {
                deps.push(SbomDependency {
                    name: name.clone(),
                    version_req: ver.as_str().map(|s| s.to_owned()),
                    ecosystem: "npm".into(),
                });
            }
        }
    }

    ExtractedManifest {
        dependencies: deps,
        license: parse_npm_license(&val),
        readme: None,
    }
}

/// `"license": "MIT"`, or the pre-2015 `{"type": "MIT", "url": …}` object.
///
/// The deprecated `"licenses": [{…}]` array is read too, joined with ` OR ` —
/// npm's own documented meaning for it is a choice between them, which is what
/// the SPDX operator says. Packages this old are still in every lockfile that
/// has not been regenerated, so leaving them unknown would silently exempt them
/// from the gate.
fn parse_npm_license(val: &serde_json::Value) -> Option<String> {
    if let Some(s) = val.get("license").and_then(|v| v.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_owned());
        }
    }
    if let Some(t) = val
        .get("license")
        .and_then(|v| v.get("type"))
        .and_then(|v| v.as_str())
    {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t.to_owned());
        }
    }
    let joined: Vec<String> = val
        .get("licenses")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    entry
                        .as_str()
                        .or_else(|| entry.get("type").and_then(|t| t.as_str()))
                })
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// npm was the only extractor that constrained position, and it still was
    /// not enough: `aaa/package.json` sits at the same depth as the real one and
    /// won on tar order. Two at that depth is ambiguous, not first-wins.
    #[test]
    fn a_second_manifest_at_the_package_depth_is_ambiguous() {
        let data = targz(&[
            ("aaa/package.json", br#"{"license":"MIT"}"#),
            ("package/package.json", br#"{"license":"GPL-3.0-only"}"#),
        ]);
        assert_eq!(extract_npm_manifest(&data), ExtractedManifest::default());
    }

    #[test]
    fn the_ordinary_single_manifest_tarball_still_reads() {
        let data = targz(&[("package/package.json", br#"{"license":"GPL-3.0-only"}"#)]);
        assert_eq!(
            extract_npm_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    #[test]
    fn parse_npm_package_json_basic() {
        let json = r#"{"dependencies":{"express":"4.0.0"},"peerDependencies":{"react":"18"}}"#;
        let deps = parse_npm_package_json(json).dependencies;
        assert_eq!(deps.len(), 2);
        let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"express"));
        assert!(names.contains(&"react"));
    }

    #[test]
    fn parse_npm_package_json_invalid_is_empty() {
        assert_eq!(
            parse_npm_package_json("not json"),
            ExtractedManifest::default()
        );
        // Valid JSON without dependency keys → no deps.
        assert!(parse_npm_package_json(r#"{"name":"x"}"#)
            .dependencies
            .is_empty());
    }

    #[test]
    fn parse_npm_license_plain_string() {
        let m = parse_npm_package_json(r#"{"license":"MIT"}"#);
        assert_eq!(m.license.as_deref(), Some("MIT"));
    }

    /// The pre-2015 object form is still in published packages.
    #[test]
    fn parse_npm_license_object_form() {
        let m = parse_npm_package_json(r#"{"license":{"type":"ISC","url":"http://x"}}"#);
        assert_eq!(m.license.as_deref(), Some("ISC"));
    }

    /// The deprecated array means "any of these", which is SPDX `OR`.
    #[test]
    fn parse_npm_license_deprecated_array_joins_with_or() {
        let m = parse_npm_package_json(r#"{"licenses":[{"type":"MIT"},{"type":"GPL-3.0"}]}"#);
        assert_eq!(m.license.as_deref(), Some("MIT OR GPL-3.0"));
    }

    #[test]
    fn parse_npm_license_absent_is_none() {
        assert_eq!(parse_npm_package_json(r#"{"name":"x"}"#).license, None);
        assert_eq!(parse_npm_package_json(r#"{"license":"  "}"#).license, None);
    }

    /// Build a gzipped npm-style tarball containing `package/package.json`.
    fn npm_tgz(package_json: &[u8]) -> Bytes {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_size(package_json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "package/package.json", package_json)
                .unwrap();
            builder.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_buf).unwrap();
        Bytes::from(gz.finish().unwrap())
    }

    #[test]
    fn extract_npm_deps_reads_top_level_package_json() {
        let data = npm_tgz(br#"{"license":"MIT","dependencies":{"express":"4.0.0"}}"#);
        let manifest = extract_npm_manifest(&data);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].name, "express");
        assert_eq!(manifest.dependencies[0].ecosystem, "npm");
        assert_eq!(
            manifest.dependencies[0].version_req.as_deref(),
            Some("4.0.0")
        );
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn extract_npm_deps_on_non_gzip_is_empty() {
        assert_eq!(
            extract_npm_manifest(&Bytes::from_static(b"not a gzip stream")),
            ExtractedManifest::default()
        );
    }

    use super::super::readme::fixtures::targz;

    /// The tarball's README is what answers for the versions whose packument
    /// carries none — which is why npm reads both.
    #[test]
    fn the_tarballs_readme_comes_back_with_the_manifest() {
        let data = targz(&[
            (
                "package/package.json",
                br#"{"name":"x","license":"MIT","dependencies":{"y":"^1"}}"#.as_slice(),
            ),
            ("package/README.md", b"# x".as_slice()),
        ]);
        let manifest = extract_npm_manifest(&data);
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.readme.unwrap().content, "# x");
    }

    /// A README inside a bundled dependency is not this package's.
    #[test]
    fn a_bundled_dependencys_readme_is_not_the_packages_own() {
        let data = targz(&[
            ("package/package.json", br#"{"name":"x"}"#.as_slice()),
            (
                "package/node_modules/dep/README.md",
                b"# somebody else".as_slice(),
            ),
        ]);
        assert!(extract_npm_manifest(&data).readme.is_none());
    }
}
