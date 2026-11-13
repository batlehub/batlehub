//! Go's release document (`go.dev/dl/?mode=json&include=all`), read once for
//! every place the `godist` kind needs it (RFC 0048 §6.7).
//!
//! The document is an array of releases, newest first, each with its files:
//!
//! ```json
//! [{"version": "go1.25.4", "stable": true,
//!   "files": [{"filename": "go1.25.4.linux-amd64.tar.gz", "os": "linux",
//!              "arch": "amd64", "version": "go1.25.4",
//!              "sha256": "9fa5…", "size": 59766394, "kind": "archive"}]}]
//! ```
//!
//! It carries a `sha256` and a `size` for every file and **no date**, which is
//! why a `godist` registry refuses a release-age gate at load. The coordinate
//! is `go@{version}` with the `go` prefix dropped (`1.25.4`), as mise spells it
//! (`mise 2026.9.5 src/plugins/core/go.rs:384-387` builds
//! `{go.download_mirror}/go{version}.{os}-{arch}.{ext}`).

use serde::{Deserialize, Serialize};

/// The one package a `godist` registry serves.
pub const GO_PACKAGE: &str = "go";

/// The release document read when the registry names none.
pub const DEFAULT_METADATA_URL: &str = "https://go.dev/dl/?mode=json&include=all";

/// The release document past which it is refused rather than parsed: 2.3 MB
/// when probed on 2026-11-13, so 8 MiB is room for years of releases and a
/// ceiling on what a hostile upstream can make this process hold.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// One release of the document.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Release {
    /// `go1.25.4`, as the document spells it.
    pub version: String,
    #[serde(default)]
    pub stable: bool,
    #[serde(default)]
    pub files: Vec<ReleaseFile>,
}

/// One file of a release.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseFile {
    pub filename: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    /// Bare lowercase hex — the form `PackageMetadata::checksum` takes.
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub kind: String,
}

/// Parse the release document. `None` for anything over
/// [`MAX_DOCUMENT_BYTES`] or not of the document's shape.
pub fn parse_releases(body: &[u8]) -> Option<Vec<Release>> {
    if body.len() > MAX_DOCUMENT_BYTES {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// `go1.25.4` → `1.25.4`; anything without the prefix is returned as it is.
pub fn bare_version(version: &str) -> &str {
    version.strip_prefix("go").unwrap_or(version)
}

/// A downloadable file name, split: `go1.25.4.linux-amd64.tar.gz.sha256` →
/// `("1.25.4", "go1.25.4.linux-amd64.tar.gz", true)`.
///
/// Only `go{version}.{os}-{arch}.{tar.gz|zip}` and its `.sha256` are accepted —
/// what mise asks for — so nothing else ever reaches a storage key or an
/// upstream URL. The version is `[0-9a-z.]`, which covers `1.26rc1`.
pub fn parse_file_name(file: &str) -> Option<(&str, &str, bool)> {
    let (archive, checksum) = match file.strip_suffix(".sha256") {
        Some(a) => (a, true),
        None => (file, false),
    };
    let stem = archive
        .strip_suffix(".tar.gz")
        .or_else(|| archive.strip_suffix(".zip"))?
        .strip_prefix("go")?;
    let (version, platform) = stem.rsplit_once('.')?;
    let (os, arch) = platform.split_once('-')?;
    let ok_version = !version.is_empty()
        && version.starts_with(|c: char| c.is_ascii_digit())
        && !version.contains("..")
        && version
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == '.');
    let ok_part = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
    };
    (ok_version && ok_part(os) && ok_part(arch)).then_some((version, archive, checksum))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_mise_asks_for_parse() {
        assert_eq!(
            parse_file_name("go1.25.4.linux-amd64.tar.gz"),
            Some(("1.25.4", "go1.25.4.linux-amd64.tar.gz", false))
        );
        assert_eq!(
            parse_file_name("go1.25.4.windows-arm64.zip.sha256"),
            Some(("1.25.4", "go1.25.4.windows-arm64.zip", true))
        );
        assert_eq!(
            parse_file_name("go1.26rc1.linux-armv6l.tar.gz"),
            Some(("1.26rc1", "go1.26rc1.linux-armv6l.tar.gz", false))
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "go1.25.4.src.tar.gz",
            "go1.25.4.darwin-arm64.pkg",
            "../go1.25.4.linux-amd64.tar.gz",
            "go..1.linux-amd64.tar.gz",
            "go1.25.4.linux-amd64/x.tar.gz",
            "go1.25.4.linux-AMD64.tar.gz",
            "index.json",
            "go.linux-amd64.tar.gz",
        ] {
            assert_eq!(parse_file_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_document_parses_and_is_capped() {
        let doc = br#"[{"version":"go1.25.4","stable":true,"files":[{"filename":"go1.25.4.linux-amd64.tar.gz","os":"linux","arch":"amd64","version":"go1.25.4","sha256":"9f","size":1,"kind":"archive"}]}]"#;
        let releases = parse_releases(doc).unwrap();
        assert_eq!(bare_version(&releases[0].version), "1.25.4");
        assert_eq!(releases[0].files[0].sha256, "9f");
        assert!(parse_releases(b"{}").is_none());
        assert!(parse_releases(&vec![b' '; MAX_DOCUMENT_BYTES + 1]).is_none());
    }
}
