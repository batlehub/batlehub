//! Alpine `apk` coordinates, `.PKGINFO` parsing and `APKINDEX` rendering.
//!
//! No I/O: everything here is a pure function over bytes or text, so the
//! adapter that walks the gzip members and the handler that validates a request
//! path can both use it, and both can be tested without an upstream.
//!
//! The three shapes, and why each exists (RFC 0026 §6.1):
//!
//! - [`apk_coordinate`] — `busybox-1.37.0-r20.apk` → (`busybox`, `1.37.0-r20`).
//!   This is what makes `apk` the only path-addressed kind with an identity a
//!   block list, an age gate and the explore view can act on.
//! - [`PkgInfo`] — the `key = value` control file, read out of an upload so the
//!   index entry is rendered from the package's own metadata rather than from a
//!   file name the client chose.
//! - [`index_entry`] and [`ApkIndex`] — the two directions of `APKINDEX`: one
//!   written for a locally hosted repository, one parsed to recover a package's
//!   build date from a cached upstream index.

use std::collections::HashMap;

/// The release suffix every Alpine package version ends in: `-r` and digits.
///
/// Measured over `v3.22/main/x86_64`: all 5 647 versions carry exactly one
/// dash, the one before this suffix. That is what makes [`apk_coordinate`] a
/// right-anchored split rather than a guess — and it survives the adversarial
/// case the branch actually contains, `linux-firmware-r128`, a package whose
/// *name* ends in `-r<digits>`.
fn is_release_suffix(token: &str) -> bool {
    token
        .strip_prefix('r')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Split an `.apk` file name into `(name, version)`.
///
/// `{name}-{pkgver}-r{N}.apk`, taken from the right: the last two dash-separated
/// tokens are the version, everything before them is the name. Returns `None`
/// for a name with no `-r<digits>` token, an empty name, or an empty `pkgver` —
/// the handler turns that into a `400` at the edge rather than guessing.
///
/// ```
/// use batlehub_core::services::apk::apk_coordinate;
/// assert_eq!(apk_coordinate("busybox-1.37.0-r20.apk"), Some(("busybox", "1.37.0-r20")));
/// assert_eq!(apk_coordinate("py3-requests-2.32.4-r0.apk"), Some(("py3-requests", "2.32.4-r0")));
/// assert_eq!(apk_coordinate("x-1.0.apk"), None);
/// ```
pub fn apk_coordinate(file_name: &str) -> Option<(&str, &str)> {
    let stem = file_name.strip_suffix(".apk")?;
    let (rest, release) = stem.rsplit_once('-')?;
    if !is_release_suffix(release) {
        return None;
    }
    let (name, pkgver) = rest.rsplit_once('-')?;
    if name.is_empty() || pkgver.is_empty() {
        return None;
    }
    // The version is reassembled from the two tokens rather than sliced out of
    // `stem`, so the caller cannot receive a borrow that spans the separator it
    // did not intend.
    Some((name, &stem[name.len() + 1..]))
}

/// The `.PKGINFO` control file: ordered `key = value` lines, keys repeatable.
///
/// Ordered and repeat-preserving for the same reason `PacmanPackage::fields` is:
/// `depend`, `provides` and `install_if` all appear many times, and the index
/// entry renders them in file order.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PkgInfo {
    pub fields: Vec<(String, String)>,
}

impl PkgInfo {
    /// Parse the `key = value` body. Comment lines (`#`) and blank lines are
    /// skipped; a line with no `=` is skipped rather than failing, because
    /// `abuild` has historically written a bare trailing newline.
    pub fn parse(bytes: &[u8]) -> Self {
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
        Self { fields }
    }

    /// First value for a key.
    pub fn first(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// All values for a repeatable key, in file order.
    pub fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.fields
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn name(&self) -> Option<&str> {
        self.first("pkgname")
    }
    pub fn version(&self) -> Option<&str> {
        self.first("pkgver")
    }
    pub fn arch(&self) -> Option<&str> {
        self.first("arch")
    }

    /// The download file name this package is stored under: `{name}-{version}.apk`,
    /// built from the package's own metadata and never from the client's.
    pub fn file_name(&self) -> Option<String> {
        Some(format!("{}-{}.apk", self.name()?, self.version()?))
    }
}

/// One rendered `APKINDEX` block, in `apk_pkg_write_index_entry`'s field order
/// (`package.c:1128–1162`).
///
/// The order matters for reproducing a fixture byte for byte; it does *not*
/// matter to apk, which parses by key. `v3.22/main` contains 21 distinct field
/// orders — `k:` precedes `D:` in some entries — so [`ApkIndex::parse`] must
/// not assume one, and does not.
///
/// `identity` is the `C:` value: `Q1` followed by base64 of the SHA-1 over the
/// control member's *compressed* bytes. That is the trust anchor apk checks at
/// install (`APK_SIGN_VERIFY_IDENTITY`), and the package's own `.SIGN.*` member
/// is never consulted for a repository install — which is why an unsigned
/// `apk mkpkg` package installs fine from a signed index (RFC 0026 §2.5).
pub fn index_entry(info: &PkgInfo, identity: &str, size: u64) -> String {
    let mut out = String::with_capacity(512);
    let mut put = |key: &str, value: &str| {
        if !value.is_empty() {
            out.push_str(key);
            out.push_str(value);
            out.push('\n');
        }
    };

    put("C:", identity);
    put("P:", info.name().unwrap_or_default());
    put("V:", info.version().unwrap_or_default());
    put("A:", info.arch().unwrap_or_default());
    put("S:", &size.to_string());
    put("I:", info.first("size").unwrap_or_default());
    put("T:", info.first("pkgdesc").unwrap_or_default());
    put("U:", info.first("url").unwrap_or_default());
    put("L:", info.first("license").unwrap_or_default());
    put("o:", info.first("origin").unwrap_or_default());
    put("m:", info.first("maintainer").unwrap_or_default());
    put("t:", info.first("builddate").unwrap_or_default());
    put("c:", info.first("commit").unwrap_or_default());

    // The repeatable fields are space-joined onto one line, which is how apk
    // writes them and how its parser reads them back.
    let join = |key: &str| -> String { info.all(key).collect::<Vec<_>>().join(" ") };
    put("D:", &join("depend"));
    put("p:", &join("provides"));
    put("i:", &join("install_if"));
    put("k:", info.first("provider_priority").unwrap_or_default());

    out
}

/// Render a whole `APKINDEX` body from already-rendered entries.
///
/// Blocks are separated by a blank line and the document ends with one, which
/// is what `apk index` writes and what `ApkIndex::parse` expects.
pub fn render_index(entries: &[String]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(entry);
        out.push('\n');
    }
    out
}

/// The parsed view of an upstream `APKINDEX`: `(name, version)` → build time.
///
/// Deliberately narrow. The age gate wants `t:` and nothing else, so this keeps
/// one `i64` per package rather than every field of a 2.26 MB document — the
/// index for `v3.22/main/x86_64` holds 5 647 packages and all 5 647 carry a
/// `t:`, so the map is dense and small where the document is not.
#[derive(Debug, Clone, Default)]
pub struct ApkIndex {
    built_at: HashMap<(String, String), i64>,
}

impl ApkIndex {
    /// One linear pass over the text, reading only `P:`, `V:` and `t:`.
    ///
    /// Field order is not assumed: a block's fields are collected until the
    /// blank line that ends it, then recorded. A block missing a name or a
    /// version is skipped rather than failing the parse — a single malformed
    /// entry must not cost the age gate every other package's date.
    pub fn parse(text: &str) -> Self {
        let mut built_at = HashMap::new();
        let (mut name, mut version, mut t) = (None, None, None);

        let mut flush =
            |name: &mut Option<String>, version: &mut Option<String>, t: &mut Option<i64>| {
                if let (Some(n), Some(v)) = (name.take(), version.take()) {
                    if let Some(secs) = t.take() {
                        built_at.insert((n, v), secs);
                    }
                }
                *t = None;
            };

        for line in text.lines() {
            if line.is_empty() {
                flush(&mut name, &mut version, &mut t);
                continue;
            }
            match line.split_at_checked(2) {
                Some(("P:", rest)) => name = Some(rest.to_owned()),
                Some(("V:", rest)) => version = Some(rest.to_owned()),
                Some(("t:", rest)) => t = rest.parse::<i64>().ok(),
                _ => {}
            }
        }
        flush(&mut name, &mut version, &mut t);

        Self { built_at }
    }

    /// The unix build time of one package, if the index lists it.
    ///
    /// `None` means the cached index does not carry this version — the case
    /// `release_age_gate`'s mandatory `deny_missing_timestamp` exists to
    /// answer (RFC 0026 §4.5).
    pub fn built_at(&self, name: &str, version: &str) -> Option<i64> {
        self.built_at
            .get(&(name.to_owned(), version.to_owned()))
            .copied()
    }

    pub fn len(&self) -> usize {
        self.built_at.len()
    }

    pub fn is_empty(&self) -> bool {
        self.built_at.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_splits_a_plain_name() {
        assert_eq!(
            apk_coordinate("busybox-1.37.0-r20.apk"),
            Some(("busybox", "1.37.0-r20"))
        );
    }

    #[test]
    fn coordinate_splits_a_name_containing_dashes() {
        assert_eq!(
            apk_coordinate("py3-requests-2.32.4-r0.apk"),
            Some(("py3-requests", "2.32.4-r0"))
        );
    }

    /// The adversarial case `v3.22/main` actually contains: a package whose own
    /// *name* ends in `-r<digits>`. Taking the version from the right is what
    /// keeps this unambiguous, and it is the reason this test names a real
    /// package rather than an invented string (RFC 0026 §10).
    #[test]
    fn coordinate_splits_a_name_ending_in_a_release_suffix() {
        assert_eq!(
            apk_coordinate("linux-firmware-r128-20250613-r0.apk"),
            Some(("linux-firmware-r128", "20250613-r0"))
        );
    }

    #[test]
    fn coordinate_rejects_a_name_with_no_release_suffix() {
        assert_eq!(apk_coordinate("x-1.0.apk"), None);
        assert_eq!(apk_coordinate("noversion.apk"), None);
        // `-rc1` is not `-r<digits>`.
        assert_eq!(apk_coordinate("thing-1.0-rc1.apk"), None);
    }

    #[test]
    fn coordinate_rejects_a_non_apk_name() {
        assert_eq!(apk_coordinate("busybox-1.37.0-r20.tar.gz"), None);
        assert_eq!(apk_coordinate("APKINDEX.tar.gz"), None);
    }

    #[test]
    fn coordinate_rejects_an_empty_half() {
        assert_eq!(apk_coordinate("-1.0-r0.apk"), None);
        assert_eq!(apk_coordinate("busybox--r0.apk"), None);
    }

    #[test]
    fn pkginfo_parses_repeated_keys_in_order() {
        let info = PkgInfo::parse(
            b"# Generated by abuild\npkgname = busybox\npkgver = 1.37.0-r20\n\
              arch = x86_64\ndepend = so:libc.musl-x86_64.so.1\ndepend = so:libz.so.1\n",
        );
        assert_eq!(info.name(), Some("busybox"));
        assert_eq!(info.version(), Some("1.37.0-r20"));
        assert_eq!(info.arch(), Some("x86_64"));
        assert_eq!(
            info.all("depend").collect::<Vec<_>>(),
            vec!["so:libc.musl-x86_64.so.1", "so:libz.so.1"]
        );
        assert_eq!(info.file_name().as_deref(), Some("busybox-1.37.0-r20.apk"));
    }

    /// Reproduces the real `busybox-1.37.0-r20` block from `v3.22/main/x86_64`
    /// byte for byte, `C:` included — the identity being the one measured
    /// against the mirror (RFC 0026 §13).
    #[test]
    fn index_entry_reproduces_the_upstream_block() {
        let info = PkgInfo::parse(
            b"pkgname = busybox\npkgver = 1.37.0-r20\npkgdesc = Size optimized toolbox of many common UNIX utilities\n\
              url = https://busybox.net/\nbuilddate = 1763764856\npackager = Buildozer\n\
              size = 817380\narch = x86_64\norigin = busybox\n\
              commit = 2e97d754a30d558f15524b8e422303c8a96832df\n\
              maintainer = S\xc3\xb6ren Tempel <soeren+alpine@soeren-tempel.net>\n\
              license = GPL-2.0-only\ndepend = so:libc.musl-x86_64.so.1\n\
              provides = cmd:busybox=1.37.0-r20\n",
        );
        let rendered = index_entry(&info, "Q1Pp11KIKAs8SS6R8w4SbCQA0XAbM=", 506116);
        let expected = "C:Q1Pp11KIKAs8SS6R8w4SbCQA0XAbM=\n\
             P:busybox\n\
             V:1.37.0-r20\n\
             A:x86_64\n\
             S:506116\n\
             I:817380\n\
             T:Size optimized toolbox of many common UNIX utilities\n\
             U:https://busybox.net/\n\
             L:GPL-2.0-only\n\
             o:busybox\n\
             m:Sören Tempel <soeren+alpine@soeren-tempel.net>\n\
             t:1763764856\n\
             c:2e97d754a30d558f15524b8e422303c8a96832df\n\
             D:so:libc.musl-x86_64.so.1\n\
             p:cmd:busybox=1.37.0-r20\n";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn index_entry_omits_absent_fields() {
        let info = PkgInfo::parse(b"pkgname = min\npkgver = 1.0-r0\narch = noarch\n");
        let rendered = index_entry(&info, "Q1xxx=", 12);
        assert_eq!(rendered, "C:Q1xxx=\nP:min\nV:1.0-r0\nA:noarch\nS:12\n");
    }

    #[test]
    fn index_parses_name_version_and_time() {
        let text = "C:Q1a=\nP:busybox\nV:1.37.0-r20\nt:1763764856\n\n\
                    C:Q1b=\nP:curl\nV:8.14.1-r3\nt:1749000000\n\n";
        let index = ApkIndex::parse(text);
        assert_eq!(index.len(), 2);
        assert_eq!(index.built_at("busybox", "1.37.0-r20"), Some(1763764856));
        assert_eq!(index.built_at("curl", "8.14.1-r3"), Some(1749000000));
        assert_eq!(index.built_at("busybox", "9.9.9-r0"), None);
    }

    /// `v3.22/main` contains 21 distinct field orders — `k:` before `D:` among
    /// them — so the parser reads by key and never by position.
    #[test]
    fn index_parse_does_not_assume_field_order() {
        let text = "t:1700000000\nV:2.0-r1\nP:oddly-ordered\nC:Q1z=\n\n";
        let index = ApkIndex::parse(text);
        assert_eq!(index.built_at("oddly-ordered", "2.0-r1"), Some(1700000000));
    }

    /// A malformed block must not cost the age gate every other package's date.
    #[test]
    fn index_parse_skips_an_entry_with_no_timestamp() {
        let text = "P:dated\nV:1.0-r0\nt:42\n\nP:undated\nV:1.0-r0\n\nP:also\nV:2.0-r0\nt:43\n\n";
        let index = ApkIndex::parse(text);
        assert_eq!(index.len(), 2);
        assert_eq!(index.built_at("dated", "1.0-r0"), Some(42));
        assert_eq!(index.built_at("undated", "1.0-r0"), None);
        assert_eq!(index.built_at("also", "2.0-r0"), Some(43));
    }

    #[test]
    fn index_parse_tolerates_a_missing_trailing_blank_line() {
        let index = ApkIndex::parse("P:last\nV:1.0-r0\nt:7\n");
        assert_eq!(index.built_at("last", "1.0-r0"), Some(7));
    }

    #[test]
    fn render_index_separates_blocks_with_a_blank_line() {
        let a = index_entry(
            &PkgInfo::parse(b"pkgname = a\npkgver = 1.0-r0\narch = x86_64\n"),
            "Q1a=",
            1,
        );
        let b = index_entry(
            &PkgInfo::parse(b"pkgname = b\npkgver = 2.0-r0\narch = x86_64\n"),
            "Q1b=",
            2,
        );
        let doc = render_index(&[a, b]);
        assert!(doc.contains("\n\nC:Q1b="), "blocks separated: {doc:?}");
        // Round-trips through the parser it is written for.
        assert_eq!(ApkIndex::parse(&doc).len(), 0, "no t: in these fixtures");
    }
}
