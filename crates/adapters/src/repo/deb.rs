//! Debian `.deb` parsing and APT repository index generation.
//!
//! A `.deb` is an `ar(5)` archive containing `debian-binary`, a
//! `control.tar.{gz,xz,zst}` (whose `./control` file is the deb822 metadata), and
//! a `data.tar.*` payload. To host an APT repo we extract the control fields and
//! compute the package checksums, then regenerate the `Packages` index (one
//! deb822 stanza per package) and the per-suite `Release` file. Signing of the
//! `Release` file is handled by [`super::openpgp`].

use std::io::Read;

use sha2::{Digest, Sha256};

use batlehub_core::error::CoreError;

/// Parsed metadata for a single uploaded `.deb`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DebPackage {
    /// Ordered control fields as they appeared in `./control`.
    pub control: Vec<(String, String)>,
    pub size: u64,
    pub sha256: String,
}

impl DebPackage {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.control
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn name(&self) -> Option<&str> {
        self.field("Package")
    }
    pub fn version(&self) -> Option<&str> {
        self.field("Version")
    }
    pub fn architecture(&self) -> Option<&str> {
        self.field("Architecture")
    }
}

/// Parse a `.deb`: extract `./control` and compute the archive checksums.
pub fn parse_deb(bytes: &[u8]) -> Result<DebPackage, CoreError> {
    let control_tar = extract_control_member(bytes)?;
    let control = parse_control_stanza(&control_tar)?;

    Ok(DebPackage {
        control,
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
    })
}

/// Find and decompress the `control.tar.*` member, returning the `./control`
/// file's raw bytes.
fn extract_control_member(bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut archive = ar::Archive::new(std::io::Cursor::new(bytes));
    while let Some(entry) = archive.next_entry() {
        let mut entry =
            entry.map_err(|e| CoreError::InvalidInput(format!("invalid .deb ar archive: {e}")))?;
        let id = String::from_utf8_lossy(entry.header().identifier()).to_string();
        let id = id.trim_end_matches('/'); // GNU ar appends '/'
        if let Some(ext) = id.strip_prefix("control.tar") {
            let mut raw = Vec::new();
            entry
                .read_to_end(&mut raw)
                .map_err(|e| CoreError::InvalidInput(format!("reading control member: {e}")))?;
            let tar_bytes = decompress(ext, &raw)?;
            return read_control_from_tar(&tar_bytes);
        }
    }
    Err(CoreError::InvalidInput(
        "no control.tar member found in .deb".into(),
    ))
}

/// Decompress a control tarball based on the `control.tar` suffix (`.gz`/`.xz`/
/// `.zst`/empty).
fn decompress(ext: &str, raw: &[u8]) -> Result<Vec<u8>, CoreError> {
    let out = match ext {
        "" => raw.to_vec(),
        ".gz" => {
            let mut d = flate2::read::GzDecoder::new(raw);
            let mut out = Vec::new();
            d.read_to_end(&mut out)
                .map_err(|e| CoreError::InvalidInput(format!("gz control: {e}")))?;
            out
        }
        ".xz" => {
            let mut out = Vec::new();
            lzma_rs::xz_decompress(&mut std::io::Cursor::new(raw), &mut out)
                .map_err(|e| CoreError::InvalidInput(format!("xz control: {e}")))?;
            out
        }
        ".zst" => zstd::decode_all(raw)
            .map_err(|e| CoreError::InvalidInput(format!("zstd control: {e}")))?,
        other => {
            return Err(CoreError::InvalidInput(format!(
                "unsupported control compression '{other}'"
            )))
        }
    };
    Ok(out)
}

/// Read the `control` entry out of a decompressed control tarball.
fn read_control_from_tar(tar_bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    let entries = archive
        .entries()
        .map_err(|e| CoreError::InvalidInput(format!("control tar: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| CoreError::InvalidInput(format!("control tar: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CoreError::InvalidInput(format!("control tar path: {e}")))?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        if path == "control" {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|e| CoreError::InvalidInput(format!("reading control: {e}")))?;
            return Ok(out);
        }
    }
    Err(CoreError::InvalidInput(
        "no ./control file in control.tar".into(),
    ))
}

/// Parse a single deb822 stanza (RFC822-like; continuation lines begin with a
/// space or tab).
fn parse_control_stanza(bytes: &[u8]) -> Result<Vec<(String, String)>, CoreError> {
    let text = String::from_utf8_lossy(bytes);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation of the previous field.
            if let Some(last) = fields.last_mut() {
                last.1.push('\n');
                last.1.push_str(line);
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            // deb822 §"Syntax": a field name is printable US-ASCII without
            // space or colon, and starts with neither `#` nor `-`. Anything
            // else re-renders into the `Packages` index as a line apt cannot
            // read — `": x"` became an empty-named field — so it is refused
            // here, at publish, rather than shipped to every client.
            if k.is_empty()
                || k.starts_with(['#', '-'])
                || !k.bytes().all(|b| (0x21..=0x7e).contains(&b))
            {
                return Err(CoreError::InvalidInput(format!(
                    "control field name {k:?} is not a valid deb822 field name"
                )));
            }
            // A repeated field is ambiguous: `name()` reads the first `Package`
            // and authorizes the publish against it, while the index carries
            // both and a client may honour the other — publishing under a name
            // one owns while the index advertises someone else's.
            if fields.iter().any(|(seen, _)| seen.eq_ignore_ascii_case(k)) {
                return Err(CoreError::InvalidInput(format!(
                    "control field '{k}' appears more than once"
                )));
            }
            fields.push((k.to_string(), v.trim().to_string()));
        }
    }
    if fields.is_empty() {
        return Err(CoreError::InvalidInput("empty control stanza".into()));
    }
    Ok(fields)
}

/// The pool path (relative to the repo root) where a `.deb` is stored, e.g.
/// `pool/{component}/{prefix}/{name}/{name}_{version}_{arch}.deb`.
pub fn pool_path(component: &str, pkg: &DebPackage) -> Result<String, CoreError> {
    let name = pkg
        .name()
        .ok_or_else(|| CoreError::InvalidInput("control is missing Package".into()))?;
    let version = pkg
        .version()
        .ok_or_else(|| CoreError::InvalidInput("control is missing Version".into()))?;
    let arch = pkg.architecture().unwrap_or("all");
    // Debian groups by first letter (or `libX` prefix); a single letter is enough
    // for a functional pool layout.
    let prefix = name.chars().next().unwrap_or('_');
    let file_version = file_version(version);
    Ok(format!(
        "pool/{component}/{prefix}/{name}/{name}_{file_version}_{arch}.deb"
    ))
}

/// The version as it appears in a pool file name: the epoch (`1:2.3`) stripped,
/// as dpkg and the Debian archive do.
///
/// So `1:2.3` and `2.3` of one package share a pool file, and the handler
/// refuses the second of them (`deb_publish`) the way the archive does,
/// rather than letting two index entries name one file with two checksums.
pub fn file_version(version: &str) -> &str {
    version.split_once(':').map(|(_, v)| v).unwrap_or(version)
}

/// The publish-time edge checks for a `.deb` going into `{distribution}/
/// {component}`, beyond the parse itself.
///
/// - `distribution` and `component` are single segments: the sidecar key is
///   `_index/deb/{distribution}/{component}/{arch}/…` and `regenerate_deb`
///   cuts it on `/`, so a `/` in either re-groups other suites' packages.
/// - The name is a single segment too, and name, version and architecture
///   carry no `_`: Debian forbids it in all three, and the pool file is
///   `{name}_{version}_{arch}.deb` — `(a, 1_x, y)` and `(a, 1, x_y)` were one
///   file, and the sidecar `{name}_{version}` made `(a_1, 2)` and `(a, 1_2)`
///   one entry across two owners.
pub fn check_coordinate(
    distribution: &str,
    component: &str,
    pkg: &DebPackage,
) -> Result<(), CoreError> {
    use batlehub_core::services::{validate_coordinate, validate_path_segment};
    validate_path_segment("distribution", distribution)?;
    validate_path_segment("component", component)?;
    let name = pkg
        .name()
        .ok_or_else(|| CoreError::InvalidInput("control is missing Package".into()))?;
    let version = pkg
        .version()
        .ok_or_else(|| CoreError::InvalidInput("control is missing Version".into()))?;
    let arch = pkg.architecture().unwrap_or("all");
    validate_coordinate(name, version, None)?;
    validate_path_segment("package name", name)?;
    validate_path_segment("architecture", arch)?;
    // Policy §5.6.1: a name starts with an alphanumeric. Its first character
    // is a pool directory of its own, so `.a` put a `.` segment in the pool
    // path, which the storage guard refuses only after the policy gate ran.
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err(CoreError::InvalidInput(format!(
            "package name '{name}' must start with a letter or digit"
        )));
    }
    for (kind, value) in [
        ("package name", name),
        ("version", version),
        ("architecture", arch),
    ] {
        if value.contains('_') {
            return Err(CoreError::InvalidInput(format!(
                "{kind} '{value}' must not contain '_'"
            )));
        }
    }
    Ok(())
}

/// Render one `Packages` stanza for a package stored at `filename` (pool path).
/// Drops any control fields APT recomputes, then appends Filename/Size/checksums.
pub fn packages_stanza(pkg: &DebPackage, filename: &str) -> String {
    let mut out = String::new();
    for (k, v) in &pkg.control {
        // Repository-level fields, not the uploader's to set: `filename`, `size`
        // and `sha256` we emit ourselves below, and `md5sum`/`sha1` are dropped
        // outright — the format makes them optional and forbids a client from
        // trusting them, so an upload does not get to reintroduce one.
        if matches!(
            k.to_ascii_lowercase().as_str(),
            "filename" | "size" | "md5sum" | "sha1" | "sha256"
        ) {
            continue;
        }
        out.push_str(&format!("{k}: {v}\n"));
    }
    out.push_str(&format!("Filename: {filename}\n"));
    out.push_str(&format!("Size: {}\n", pkg.size));
    out.push_str(&format!("SHA256: {}\n", pkg.sha256));
    out
}

/// Join package stanzas into a `Packages` file (stanzas separated by a blank line).
pub fn generate_packages(stanzas: &[String]) -> String {
    stanzas.join("\n")
}

/// A generated index file to be referenced from `Release`.
pub struct ReleaseFile {
    /// Path relative to `dists/{suite}/`, e.g. `main/binary-amd64/Packages`.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

impl ReleaseFile {
    pub fn new(path: impl Into<String>, content: &[u8]) -> Self {
        Self {
            path: path.into(),
            size: content.len() as u64,
            sha256: hex::encode(Sha256::digest(content)),
        }
    }
}

/// Metadata used to render the `Release` file header.
pub struct ReleaseMeta<'a> {
    pub origin: &'a str,
    pub label: &'a str,
    pub suite: &'a str,
    pub codename: &'a str,
    pub architectures: &'a [String],
    pub components: &'a [String],
    pub date: &'a str,
}

/// Render the `Release` file body (the text that gets clear-signed into
/// `InRelease` and detached-signed into `Release.gpg`).
pub fn generate_release(meta: &ReleaseMeta, files: &[ReleaseFile]) -> String {
    let mut out = String::new();
    out.push_str(&format!("Origin: {}\n", meta.origin));
    out.push_str(&format!("Label: {}\n", meta.label));
    out.push_str(&format!("Suite: {}\n", meta.suite));
    out.push_str(&format!("Codename: {}\n", meta.codename));
    out.push_str(&format!("Date: {}\n", meta.date));
    out.push_str(&format!(
        "Architectures: {}\n",
        meta.architectures.join(" ")
    ));
    out.push_str(&format!("Components: {}\n", meta.components.join(" ")));
    out.push_str("Acquire-By-Hash: no\n");

    out.push_str("SHA256:\n");
    for f in files {
        out.push_str(&format!(" {} {} {}\n", f.sha256, f.size, f.path));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal `.deb` (ar archive with debian-binary + control.tar.gz).
    fn make_deb(control: &str) -> Vec<u8> {
        // control.tar.gz containing ./control
        let mut tar_buf = Vec::new();
        {
            let mut tb = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path("./control").unwrap();
            header.set_size(control.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append(&header, control.as_bytes()).unwrap();
            tb.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            enc.write_all(&tar_buf).unwrap();
            enc.finish().unwrap();
        }

        let mut deb = Vec::new();
        {
            let mut builder = ar::Builder::new(&mut deb);
            let db = b"2.0\n";
            builder
                .append(
                    &ar::Header::new(b"debian-binary".to_vec(), db.len() as u64),
                    &db[..],
                )
                .unwrap();
            builder
                .append(
                    &ar::Header::new(b"control.tar.gz".to_vec(), gz.len() as u64),
                    &gz[..],
                )
                .unwrap();
            // (No data.tar member needed for control parsing.)
        }
        deb
    }

    #[test]
    fn parse_deb_extracts_control_and_checksums() {
        let control = "Package: hello\nVersion: 1.0-1\nArchitecture: amd64\nMaintainer: me\nDescription: hi\n";
        let deb = make_deb(control);
        let pkg = parse_deb(&deb).unwrap();
        assert_eq!(pkg.name(), Some("hello"));
        assert_eq!(pkg.version(), Some("1.0-1"));
        assert_eq!(pkg.architecture(), Some("amd64"));
        assert_eq!(pkg.size, deb.len() as u64);
        assert_eq!(pkg.sha256, hex::encode(Sha256::digest(&deb)));
    }

    #[test]
    fn pool_path_strips_epoch_and_groups_by_prefix() {
        let control = "Package: hello\nVersion: 2:1.0-1\nArchitecture: amd64\n";
        let pkg = parse_deb(&make_deb(control)).unwrap();
        let path = pool_path("main", &pkg).unwrap();
        assert_eq!(path, "pool/main/h/hello/hello_1.0-1_amd64.deb");
    }

    #[test]
    fn packages_stanza_appends_repo_fields() {
        let control = "Package: hello\nVersion: 1.0-1\nArchitecture: amd64\n";
        let pkg = parse_deb(&make_deb(control)).unwrap();
        let stanza = packages_stanza(&pkg, "pool/main/h/hello/hello_1.0-1_amd64.deb");
        assert!(stanza.contains("Package: hello"));
        assert!(stanza.contains("Filename: pool/main/h/hello/hello_1.0-1_amd64.deb"));
        assert!(stanza.contains(&format!("SHA256: {}", pkg.sha256)));
        assert!(stanza.contains(&format!("Size: {}", pkg.size)));
    }

    #[test]
    fn control_field_names_must_be_deb822() {
        for control in [
            "Package: a\n: x\nVersion: 1\n",
            "Package: a\nVersion: 1\n#X: y\n",
            "Package: a\nVersion: 1\nBad Key: y\n",
            "Package: a\nVersion: 1\nX\u{1}: y\n",
        ] {
            assert!(parse_deb(&make_deb(control)).is_err(), "{control:?}");
        }
    }

    #[test]
    fn a_repeated_control_field_is_refused() {
        let control = "Package: mine\nVersion: 1\npackage: theirs\n";
        assert!(parse_deb(&make_deb(control)).is_err());
    }

    #[test]
    fn coordinate_rejects_separators_that_recut_the_pool_or_sidecar() {
        let pkg = |c: &str| parse_deb(&make_deb(c)).unwrap();
        let ok = pkg("Package: a\nVersion: 1:1.0\nArchitecture: amd64\n");
        assert!(check_coordinate("stable", "main", &ok).is_ok());
        assert!(check_coordinate("stable", "main/x", &ok).is_err());
        assert!(check_coordinate("a/b", "main", &ok).is_err());
        for bad in [
            "Package: x/x\nVersion: 1\n",
            "Package: .a\nVersion: 1\n",
            "Package: a_1\nVersion: 2\n",
            "Package: a\nVersion: 1_2\n",
            "Package: a\nVersion: 1\nArchitecture: x_y\n",
        ] {
            assert!(
                check_coordinate("stable", "main", &pkg(bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn release_references_files_with_hashes() {
        let packages = b"Package: hello\n";
        let rf = ReleaseFile::new("main/binary-amd64/Packages", packages);
        let meta = ReleaseMeta {
            origin: "BatleHub",
            label: "BatleHub",
            suite: "stable",
            codename: "stable",
            architectures: &["amd64".to_string()],
            components: &["main".to_string()],
            date: "Thu, 01 Jan 1970 00:00:00 UTC",
        };
        let release = generate_release(&meta, &[rf]);
        assert!(release.contains("Suite: stable"));
        assert!(release.contains("Components: main"));
        assert!(release.contains("main/binary-amd64/Packages"));
        assert!(release.contains(&hex::encode(Sha256::digest(packages))));
    }
}
