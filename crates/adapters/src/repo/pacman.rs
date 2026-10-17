//! Arch Linux `.pkg.tar.*` parsing and pacman repository database generation.
//!
//! A pacman package is a (zstd/xz/gzip-compressed, or plain) `tar` whose root
//! `.PKGINFO` file holds the deb822-ish `key = value` metadata. To host a pacman
//! repo we extract those fields and compute the package checksums, then render
//! one `desc` entry per package and pack them into the gzipped-tar repository
//! database (`<repo>.db`) that `pacman -Sy` downloads. Signing of the database
//! and of each package is handled by [`super::openpgp`].

use std::io::{Cursor, Read};

use sha2::{Digest, Sha256};

use batlehub_core::error::CoreError;

/// Parsed metadata for a single uploaded pacman package.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PacmanPackage {
    /// Ordered `.PKGINFO` fields. Repeated keys (`depend`, `provides`, …) are
    /// kept as separate entries, preserving file order.
    pub fields: Vec<(String, String)>,
    /// Download filename, e.g. `hello-1.0-1-x86_64.pkg.tar.zst`.
    pub filename: String,
    /// Compressed (download) size in bytes.
    pub csize: u64,
    pub sha256: String,
    /// Base64-encoded detached OpenPGP signature over the package bytes, set when
    /// the registry is signed. Embedded into the `%PGPSIG%` desc field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgpsig: Option<String>,
}

impl PacmanPackage {
    /// First value for a `.PKGINFO` key.
    fn first(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// All values for a (possibly repeated) `.PKGINFO` key, in file order.
    fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.fields
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn name(&self) -> Option<&str> {
        self.first("pkgname")
    }
    /// `pkgver` already encodes the `-pkgrel` suffix (e.g. `1.0-1`).
    pub fn version(&self) -> Option<&str> {
        self.first("pkgver")
    }
    pub fn arch(&self) -> Option<&str> {
        self.first("arch")
    }
}

/// Parse a pacman package: extract `.PKGINFO` and compute the archive checksums.
/// `filename` is the download name recorded in `%FILENAME%`.
pub fn parse_pacman(bytes: &[u8], filename: &str) -> Result<PacmanPackage, CoreError> {
    let tar_bytes = decompress(bytes)?;
    let pkginfo = read_pkginfo(&tar_bytes)?;
    let fields = parse_pkginfo(&pkginfo);
    if fields.is_empty() {
        return Err(CoreError::InvalidInput("empty .PKGINFO".into()));
    }
    Ok(PacmanPackage {
        fields,
        filename: filename.to_owned(),
        csize: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
        pgpsig: None,
    })
}

/// Hard cap on the decompressed package size. The upload is an attacker-controlled
/// archive and we expand the whole of it to reach `.PKGINFO`, so a zstd/xz/gzip
/// "bomb" could otherwise expand a tiny payload into terabytes and OOM the server.
/// Sized far above any realistic package's uncompressed contents.
const MAX_DECOMPRESSED: u64 = 2 * 1024 * 1024 * 1024;

/// `io::Write` sink that buffers into a `Vec` but errors once more than `limit`
/// bytes have been written, so a decompressor can't be driven into unbounded
/// allocation regardless of codec.
struct CappedWriter {
    buf: Vec<u8>,
    limit: u64,
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() as u64 + data.len() as u64 > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decompressed package exceeds size limit",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Decompress a package archive, detecting the codec from the leading magic bytes
/// (zstd / xz / gzip); a plain `tar` is passed through unchanged. Decompression is
/// bounded by [`MAX_DECOMPRESSED`] so an untrusted archive cannot OOM the server.
fn decompress(bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut sink = CappedWriter {
        buf: Vec::new(),
        limit: MAX_DECOMPRESSED,
    };
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        let mut dec = zstd::stream::read::Decoder::new(bytes)
            .map_err(|e| CoreError::InvalidInput(format!("zstd package: {e}")))?;
        std::io::copy(&mut dec, &mut sink)
            .map_err(|e| CoreError::InvalidInput(format!("zstd package: {e}")))?;
    } else if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        lzma_rs::xz_decompress(&mut Cursor::new(bytes), &mut sink)
            .map_err(|e| CoreError::InvalidInput(format!("xz package: {e}")))?;
    } else if bytes.starts_with(&[0x1F, 0x8B]) {
        let mut d = flate2::read::GzDecoder::new(bytes);
        std::io::copy(&mut d, &mut sink)
            .map_err(|e| CoreError::InvalidInput(format!("gz package: {e}")))?;
    } else {
        // Uncompressed tar (`.pkg.tar`): still bound the size.
        if bytes.len() as u64 > MAX_DECOMPRESSED {
            return Err(CoreError::InvalidInput("package exceeds size limit".into()));
        }
        return Ok(bytes.to_vec());
    }
    Ok(sink.buf)
}

/// Read the root `.PKGINFO` member out of a decompressed package tarball.
fn read_pkginfo(tar_bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
    let entries = archive
        .entries()
        .map_err(|e| CoreError::InvalidInput(format!("package tar: {e}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| CoreError::InvalidInput(format!("package tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CoreError::InvalidInput(format!("package tar path: {e}")))?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        if path == ".PKGINFO" {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|e| CoreError::InvalidInput(format!("reading .PKGINFO: {e}")))?;
            return Ok(out);
        }
    }
    Err(CoreError::InvalidInput(
        "no .PKGINFO file in package".into(),
    ))
}

/// Parse `.PKGINFO` (`key = value` lines; `#` comments and blank lines ignored).
fn parse_pkginfo(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    let mut fields = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            fields.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    fields
}

/// The publish-time edge checks on a pacman coordinate.
///
/// The stored file is `{name}-{version}-{arch}.pkg.tar.*` and the DB directory
/// `{name}-{version}`, both read back by splitting on `-` from the right — so
/// they are only one coordinate each if the right-hand parts have a fixed number
/// of dashes. makepkg guarantees it and this enforces it: `pkgver` here is
/// `[epoch:]pkgver-pkgrel` with exactly one `-`, and an architecture has none.
/// Without it `(a-b, 1-1)` and `(a, b-1-1)` were the same file under two names,
/// and a `/` in the name nested the file in another directory.
pub fn check_coordinate(name: &str, version: &str, arch: &str) -> Result<(), CoreError> {
    use batlehub_core::services::{validate_coordinate, validate_path_segment};
    validate_coordinate(name, version, None)?;
    validate_path_segment("package name", name)?;
    validate_path_segment("architecture", arch)?;
    let well_formed = version
        .split_once('-')
        .is_some_and(|(ver, rel)| !ver.is_empty() && !rel.is_empty() && !rel.contains('-'));
    if !well_formed {
        return Err(CoreError::InvalidInput(format!(
            "pkgver '{version}' must be '<pkgver>-<pkgrel>' with exactly one '-'"
        )));
    }
    if arch.contains('-') {
        return Err(CoreError::InvalidInput(format!(
            "architecture '{arch}' must not contain '-'"
        )));
    }
    Ok(())
}

/// Directory name a package occupies in the repo DB tar: `<name>-<version>`.
pub fn db_dir_name(pkg: &PacmanPackage) -> Option<String> {
    Some(format!("{}-{}", pkg.name()?, pkg.version()?))
}

/// The conventional download suffix for a package, derived from its compression
/// magic. `pacman` requires the recorded `%FILENAME%` extension to match the
/// actual codec, so we name the stored file from the bytes rather than trusting a
/// client-supplied filename.
pub fn download_suffix(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        ".pkg.tar.zst"
    } else if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        ".pkg.tar.xz"
    } else if bytes.starts_with(&[0x1F, 0x8B]) {
        ".pkg.tar.gz"
    } else {
        ".pkg.tar"
    }
}

fn push_single(out: &mut String, key: &str, val: Option<&str>) {
    if let Some(v) = val {
        if !v.is_empty() {
            out.push('%');
            out.push_str(key);
            out.push_str("%\n");
            out.push_str(v);
            out.push_str("\n\n");
        }
    }
}

fn push_multi<'a>(out: &mut String, key: &str, vals: impl Iterator<Item = &'a str>) {
    let vals: Vec<&str> = vals.filter(|v| !v.is_empty()).collect();
    if vals.is_empty() {
        return;
    }
    out.push('%');
    out.push_str(key);
    out.push_str("%\n");
    for v in vals {
        out.push_str(v);
        out.push('\n');
    }
    out.push('\n');
}

/// Render the `desc` database entry for a package (the `%FIELD%`-sectioned text
/// `pacman` reads from `<repo>.db`). `pkg.csize`/checksums are emitted by us; the
/// rest come from `.PKGINFO`.
pub fn desc_entry(pkg: &PacmanPackage) -> String {
    let mut out = String::new();
    push_single(&mut out, "FILENAME", Some(&pkg.filename));
    push_single(&mut out, "NAME", pkg.name());
    push_single(&mut out, "BASE", pkg.first("pkgbase"));
    push_single(&mut out, "VERSION", pkg.version());
    push_single(&mut out, "DESC", pkg.first("pkgdesc"));
    push_multi(&mut out, "GROUPS", pkg.all("group"));
    push_single(&mut out, "CSIZE", Some(&pkg.csize.to_string()));
    push_single(&mut out, "ISIZE", pkg.first("size"));
    push_single(&mut out, "SHA256SUM", Some(&pkg.sha256));
    push_single(&mut out, "PGPSIG", pkg.pgpsig.as_deref());
    push_single(&mut out, "URL", pkg.first("url"));
    push_multi(&mut out, "LICENSE", pkg.all("license"));
    push_single(&mut out, "ARCH", pkg.arch());
    push_single(&mut out, "BUILDDATE", pkg.first("builddate"));
    push_single(&mut out, "PACKAGER", pkg.first("packager"));
    push_multi(&mut out, "REPLACES", pkg.all("replaces"));
    push_multi(&mut out, "CONFLICTS", pkg.all("conflict"));
    push_multi(&mut out, "PROVIDES", pkg.all("provides"));
    push_multi(&mut out, "DEPENDS", pkg.all("depend"));
    push_multi(&mut out, "OPTDEPENDS", pkg.all("optdepend"));
    push_multi(&mut out, "MAKEDEPENDS", pkg.all("makedepend"));
    push_multi(&mut out, "CHECKDEPENDS", pkg.all("checkdepend"));
    out
}

/// Build the gzipped-tar repository database from `(dir_name, desc)` pairs, where
/// each entry becomes a `<dir_name>/desc` file. Used for both `<repo>.db` and the
/// `<repo>.files` database (the latter without per-file `%FILES%` listings).
pub fn generate_db(entries: &[(String, String)]) -> std::io::Result<Vec<u8>> {
    let mut tar_buf = Vec::new();
    {
        let mut tb = tar::Builder::new(&mut tar_buf);
        for (dir, desc) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(desc.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            // `append_data` sets the path (with GNU long-name handling) and the
            // checksum, so package dirs longer than 100 bytes still pack cleanly.
            tb.append_data(&mut header, format!("{dir}/desc"), desc.as_bytes())?;
        }
        tb.finish()?;
    }
    super::gzip(&tar_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_parts_must_split_back_from_the_right() {
        assert!(check_coordinate("hello-world", "1:1.0-1", "x86_64").is_ok());
        for (name, version, arch) in [
            ("a", "b-1-1", "x86_64"),
            ("a", "1.0", "x86_64"),
            ("a", "1.0-", "x86_64"),
            ("a", "1-0", "x86-64"),
            ("a/b", "1-0", "any"),
            ("a", "1-0", "x/y"),
        ] {
            assert!(
                check_coordinate(name, version, arch).is_err(),
                "{name} {version} {arch}"
            );
        }
    }

    const PKGINFO: &str = "# Generated by makepkg\npkgname = hello\npkgbase = hello\npkgver = 1.0-1\npkgdesc = A greeting\nurl = https://example.com\nbuilddate = 1700000000\npackager = me <me@example.com>\nsize = 4096\narch = x86_64\nlicense = MIT\ndepend = glibc\ndepend = bash\nprovides = hello=1.0\n";

    /// Build a minimal `.pkg.tar.zst` (tar with a root `.PKGINFO`, zstd-encoded).
    fn make_pkg(pkginfo: &str) -> Vec<u8> {
        let mut tar_buf = Vec::new();
        {
            let mut tb = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path(".PKGINFO").unwrap();
            header.set_size(pkginfo.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append(&header, pkginfo.as_bytes()).unwrap();
            tb.finish().unwrap();
        }
        zstd::encode_all(Cursor::new(tar_buf), 0).unwrap()
    }

    #[test]
    fn parse_pacman_extracts_fields_and_checksums() {
        let bytes = make_pkg(PKGINFO);
        let pkg = parse_pacman(&bytes, "hello-1.0-1-x86_64.pkg.tar.zst").unwrap();
        assert_eq!(pkg.name(), Some("hello"));
        assert_eq!(pkg.version(), Some("1.0-1"));
        assert_eq!(pkg.arch(), Some("x86_64"));
        assert_eq!(pkg.csize, bytes.len() as u64);
        assert_eq!(pkg.sha256, hex::encode(Sha256::digest(&bytes)));
        // Repeated keys accumulate.
        assert_eq!(pkg.all("depend").collect::<Vec<_>>(), vec!["glibc", "bash"]);
    }

    #[test]
    fn parse_pacman_rejects_missing_pkginfo() {
        // An empty tar (no .PKGINFO) must error rather than yield a bare package.
        let mut tar_buf = Vec::new();
        tar::Builder::new(&mut tar_buf).finish().unwrap();
        let bytes = zstd::encode_all(Cursor::new(tar_buf), 0).unwrap();
        assert!(parse_pacman(&bytes, "x.pkg.tar.zst").is_err());
    }

    #[test]
    fn desc_entry_renders_core_sections() {
        let bytes = make_pkg(PKGINFO);
        let pkg = parse_pacman(&bytes, "hello-1.0-1-x86_64.pkg.tar.zst").unwrap();
        let desc = desc_entry(&pkg);
        assert!(desc.contains("%FILENAME%\nhello-1.0-1-x86_64.pkg.tar.zst\n"));
        assert!(desc.contains("%NAME%\nhello\n"));
        assert!(desc.contains("%VERSION%\n1.0-1\n"));
        assert!(desc.contains("%ARCH%\nx86_64\n"));
        assert!(desc.contains(&format!("%SHA256SUM%\n{}\n", pkg.sha256)));
        // Multi-valued DEPENDS lists each entry on its own line.
        assert!(desc.contains("%DEPENDS%\nglibc\nbash\n"));
        // No signature configured ⇒ no PGPSIG section.
        assert!(!desc.contains("%PGPSIG%"));
    }

    #[test]
    fn generate_db_round_trips() {
        let bytes = make_pkg(PKGINFO);
        let pkg = parse_pacman(&bytes, "hello-1.0-1-x86_64.pkg.tar.zst").unwrap();
        let dir = db_dir_name(&pkg).unwrap();
        assert_eq!(dir, "hello-1.0-1");
        let db = generate_db(&[(dir, desc_entry(&pkg))]).unwrap();

        // Gunzip → untar → the `hello-1.0-1/desc` entry holds the rendered desc.
        let mut gz = flate2::read::GzDecoder::new(Cursor::new(db));
        let mut tar_bytes = Vec::new();
        gz.read_to_end(&mut tar_bytes).unwrap();
        let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
        let mut found = false;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            if path == "hello-1.0-1/desc" {
                let mut s = String::new();
                entry.read_to_string(&mut s).unwrap();
                assert!(s.contains("%NAME%\nhello\n"));
                found = true;
            }
        }
        assert!(found, "db tar missing hello-1.0-1/desc");
    }
}
