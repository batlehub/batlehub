use batlehub_core::ports::{ExtractedManifest, ExtractedReadme, SbomDependency};
use bytes::Bytes;

use super::{anchor, readme};

pub(super) fn extract_cargo_manifest(data: &Bytes) -> ExtractedManifest {
    use std::io::Read;

    let mut archive = readme::open_targz(data);

    let Ok(entries) = archive.entries() else {
        tracing::warn!("sbom: failed to parse cargo manifest, treating as no dependencies");
        return ExtractedManifest::default();
    };

    // Every `{dir}/Cargo.toml`, not the first one at any depth: a `.crate` has
    // one wrapper directory, and `aaa/vendor/Cargo.toml` placed ahead of it used
    // to decide the licence. See `anchor`.
    let mut candidates: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(path) = entry.path() else { continue };
        if !anchor::is_crate_manifest(&path.to_string_lossy()) {
            continue;
        }
        let mut reader = entry;
        let mut content = String::new();
        if reader.read_to_string(&mut content).is_err() {
            tracing::warn!("sbom: failed to parse cargo manifest, treating as no dependencies");
            return ExtractedManifest::default();
        }
        candidates.push(content);
    }

    let Some(content) = anchor::sole(candidates) else {
        return ExtractedManifest::default();
    };
    // Same decompression, three facts. The archive is open and in memory;
    // opening it again for the README would repeat the waste RFC 0004-bis §13.1
    // removed.
    ExtractedManifest {
        dependencies: parse_cargo_toml_deps(&content),
        license: parse_cargo_toml_license(&content),
        readme: cargo_readme(data, parse_cargo_toml_readme(&content).as_deref()),
    }
}

/// The README named by `[package] readme`, or the conventional one.
///
/// A `.crate` wraps everything in one `{name}-{version}/` directory, so the
/// declared path is relative to that. `readme = false` is cargo's way of saying
/// there is none, and it means none — not "fall back to the convention".
fn cargo_readme(data: &Bytes, declared: Option<&str>) -> Option<ExtractedReadme> {
    match declared {
        // `readme = false`: the crate says it has none.
        Some("false") => None,
        // A named file, matched at the wrapper directory's depth. Not anywhere
        // in the archive: `readme = "guide/README.md"` names one file, and a
        // same-named file elsewhere is a different document.
        Some(path) if path != "true" => {
            let wanted = path.trim_start_matches("./").replace('\\', "/");
            readme::readme_from_targz(data, |entry| {
                entry
                    .split_once('/')
                    .is_some_and(|(_, rest)| rest == wanted)
            })
        }
        // Absent, or `readme = true` — cargo's shorthand for "the conventional
        // one" — both mean the root `README*`.
        _ => readme::readme_from_targz(data, readme::root_readme_matcher(1)),
    }
}

/// `[package] readme = "README.md"`, `readme = true`, or `readme = false`.
///
/// Returned as the raw token rather than a parsed enum: the caller has to
/// distinguish three cases and a `bool` cannot carry a filename.
fn parse_cargo_toml_readme(content: &str) -> Option<String> {
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("readme") else {
            continue;
        };
        let Some(value) = rest.trim().strip_prefix('=') else {
            continue; // `readme-file`, or anything else starting "readme"
        };
        let value = value.trim().trim_matches('"').trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// `[package] license = "MIT OR Apache-2.0"`.
///
/// `license-file` is deliberately *not* a fallback: it names a file whose
/// contents are the licence text, so the value would be `LICENSE` — a filename
/// masquerading as an SPDX expression, which a gate would then compare against
/// its allow list and never match. A crate that only sets `license-file` is
/// correctly reported as unknown.
fn parse_cargo_toml_license(content: &str) -> Option<String> {
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("license") {
            let rest = rest.trim();
            let Some(value) = rest.strip_prefix('=') else {
                continue; // `license-file = …` and anything else starting "license"
            };
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn parse_version_from_toml_rest(rest: &str) -> String {
    if rest.starts_with('"') {
        rest.trim_matches('"').to_owned()
    } else if let Some(start) = rest.find("version = \"") {
        let after = &rest[start + 11..];
        after
            .find('"')
            .map(|end| after[..end].to_owned())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

fn parse_dep_entry(trimmed: &str) -> Option<SbomDependency> {
    let (name, rest) = trimmed.split_once('=')?;
    let name = name.trim().trim_matches('"');
    if name.is_empty() || name.starts_with('#') {
        return None;
    }
    let version = parse_version_from_toml_rest(rest.trim());
    Some(SbomDependency {
        name: name.to_owned(),
        version_req: if version.is_empty() {
            None
        } else {
            Some(version)
        },
        ecosystem: "cargo".into(),
    })
}

fn parse_cargo_toml_deps(content: &str) -> Vec<SbomDependency> {
    let mut deps = Vec::new();
    let mut in_deps = false;
    let mut in_dev_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "[dependencies]" {
            in_deps = true;
            in_dev_deps = false;
            continue;
        }
        if trimmed == "[dev-dependencies]" || trimmed == "[build-dependencies]" {
            in_deps = false;
            in_dev_deps = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_deps = false;
            in_dev_deps = false;
            continue;
        }
        if !in_deps && !in_dev_deps {
            continue;
        }
        if let Some(dep) = parse_dep_entry(trimmed) {
            deps.push(dep);
        }
    }
    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &[u8] = b"[package]\nname = \"evil\"\nlicense = \"GPL-3.0-only\"\n";
    const DECOY: &[u8] = b"[package]\nname = \"decoy\"\nlicense = \"MIT\"\n";

    /// A second `Cargo.toml` placed earlier in the tarball used to decide the
    /// licence: this returned `MIT` before the anchor, with the crate's own
    /// manifest sitting right behind it.
    #[test]
    fn a_nested_decoy_manifest_does_not_become_the_crates_own() {
        let data = targz(&[
            ("aaa/vendor/Cargo.toml", DECOY),
            ("evil-1.0.0/Cargo.toml", REAL),
        ]);
        assert_eq!(
            extract_cargo_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    /// Two manifests at the *same* depth: the anchor cannot separate them, so
    /// the archive does not name its own manifest and the answer is unknown —
    /// never the decoy's.
    #[test]
    fn two_manifests_at_the_wrapper_depth_are_ambiguous_not_first_wins() {
        let data = targz(&[("aaa/Cargo.toml", DECOY), ("evil-1.0.0/Cargo.toml", REAL)]);
        assert_eq!(extract_cargo_manifest(&data), ExtractedManifest::default());
    }

    #[test]
    fn the_ordinary_single_manifest_crate_still_reads() {
        let data = targz(&[("evil-1.0.0/Cargo.toml", REAL)]);
        assert_eq!(
            extract_cargo_manifest(&data).license.as_deref(),
            Some("GPL-3.0-only")
        );
    }

    #[test]
    fn parse_cargo_toml_basic() {
        let toml = r#"
[dependencies]
serde = "1.0"
tokio = { version = "1.0", features = ["full"] }
"#;
        let deps = parse_cargo_toml_deps(toml);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "serde");
        assert_eq!(deps[0].version_req.as_deref(), Some("1.0"));
        assert_eq!(deps[1].name, "tokio");
        assert_eq!(deps[1].version_req.as_deref(), Some("1.0"));
    }

    #[test]
    fn parse_cargo_toml_license_from_package_section() {
        let toml = "[package]\nname = \"x\"\nlicense = \"MIT OR Apache-2.0\"\n";
        assert_eq!(
            parse_cargo_toml_license(toml).as_deref(),
            Some("MIT OR Apache-2.0")
        );
    }

    /// `license-file` names a file, not an expression. Reporting `LICENSE` as
    /// the licence would give the gate a value that matches no allow list and
    /// reads as a real declaration.
    #[test]
    fn parse_cargo_toml_license_ignores_license_file() {
        let toml = "[package]\nlicense-file = \"LICENSE\"\n";
        assert_eq!(parse_cargo_toml_license(toml), None);
    }

    /// A `license` key under `[dependencies]` (or any other table) is not the
    /// crate's own declaration.
    #[test]
    fn parse_cargo_toml_license_only_reads_package_table() {
        let toml = "[dependencies]\nlicense = \"GPL-3.0\"\n";
        assert_eq!(parse_cargo_toml_license(toml), None);
    }

    #[test]
    fn parse_cargo_toml_readme_reads_the_three_shapes() {
        assert_eq!(
            parse_cargo_toml_readme("[package]\nreadme = \"README.md\"\n").as_deref(),
            Some("README.md")
        );
        assert_eq!(
            parse_cargo_toml_readme("[package]\nreadme = true\n").as_deref(),
            Some("true")
        );
        assert_eq!(
            parse_cargo_toml_readme("[package]\nreadme = false\n").as_deref(),
            Some("false")
        );
        assert_eq!(parse_cargo_toml_readme("[package]\nname = \"x\"\n"), None);
        // Another table's `readme` is not the crate's own declaration.
        assert_eq!(
            parse_cargo_toml_readme("[dependencies]\nreadme = \"X\"\n"),
            None
        );
    }

    #[test]
    fn parse_cargo_toml_license_absent_is_none() {
        assert_eq!(parse_cargo_toml_license("[package]\nname = \"x\"\n"), None);
    }

    use super::super::readme::fixtures::targz;

    /// A `.crate` wraps everything in `{name}-{version}/`, and the conventional
    /// README is read alongside the manifest — one decompression, three facts.
    #[test]
    fn the_conventional_readme_comes_back_with_the_manifest() {
        let data = targz(&[
            (
                "mylib-1.0.0/Cargo.toml",
                b"[package]\nname = \"mylib\"\nlicense = \"MIT\"\n\n[dependencies]\nserde = \"1\"\n"
                    .as_slice(),
            ),
            ("mylib-1.0.0/README.md", b"# mylib".as_slice()),
        ]);
        let manifest = extract_cargo_manifest(&data);
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.dependencies.len(), 1);
        let readme = manifest.readme.expect("README read");
        assert_eq!(readme.content, "# mylib");
        assert_eq!(readme.path, "mylib-1.0.0/README.md");
    }

    /// `readme = "guide/OVERVIEW.md"` names one file. A same-named file
    /// elsewhere is a different document, and the conventional `README.md` is
    /// not what the crate said to show.
    #[test]
    fn a_declared_readme_path_is_read_instead_of_the_convention() {
        let data = targz(&[
            (
                "mylib-1.0.0/Cargo.toml",
                b"[package]\nreadme = \"guide/OVERVIEW.md\"\n".as_slice(),
            ),
            (
                "mylib-1.0.0/README.md",
                b"# the conventional one".as_slice(),
            ),
            (
                "mylib-1.0.0/guide/OVERVIEW.md",
                b"# the declared one".as_slice(),
            ),
        ]);
        assert_eq!(
            extract_cargo_manifest(&data).readme.unwrap().content,
            "# the declared one"
        );
    }

    /// `readme = false` means the crate has none. Falling back to the
    /// convention would show a file the author said not to.
    #[test]
    fn readme_false_means_none_not_fall_back() {
        let data = targz(&[
            (
                "mylib-1.0.0/Cargo.toml",
                b"[package]\nreadme = false\n".as_slice(),
            ),
            ("mylib-1.0.0/README.md", b"# there anyway".as_slice()),
        ]);
        assert!(extract_cargo_manifest(&data).readme.is_none());
    }

    /// `readme = true` is cargo's shorthand for the conventional one.
    #[test]
    fn readme_true_means_the_conventional_one() {
        let data = targz(&[
            (
                "mylib-1.0.0/Cargo.toml",
                b"[package]\nreadme = true\n".as_slice(),
            ),
            ("mylib-1.0.0/README", b"plain prose".as_slice()),
        ]);
        let readme = extract_cargo_manifest(&data).readme.unwrap();
        assert_eq!(readme.content, "plain prose");
        // No extension declared it as markup, so it is shown escaped.
        assert_eq!(readme.format, batlehub_core::entities::ReadmeFormat::Plain);
    }

    /// A crate with no README at all reports none, and still reports its
    /// licence and dependencies.
    #[test]
    fn a_crate_without_a_readme_still_answers_for_everything_else() {
        let data = targz(&[(
            "mylib-1.0.0/Cargo.toml",
            b"[package]\nlicense = \"Apache-2.0\"\n".as_slice(),
        )]);
        let manifest = extract_cargo_manifest(&data);
        assert!(manifest.readme.is_none());
        assert_eq!(manifest.license.as_deref(), Some("Apache-2.0"));
    }
}
