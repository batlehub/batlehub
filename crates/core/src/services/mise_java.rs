//! mise's JVM catalogue (`mise-java.jdx.dev`), read for every place the
//! `mise-java` kind needs it (RFC 0048 §6.3).
//!
//! mise resolves a Java version through one document per release type, OS
//! and architecture — `{base}/jvm/{type}/{os}/{arch}.json`, an array of rows —
//! and downloads whatever URL the chosen row names (mise 2026.11.3
//! `src/plugins/core/java.rs:375-395`, `:207-234`):
//!
//! ```json
//! [{"checksum": "sha256:5f28…", "created_at": "2025-03-28T22:04:32.217430",
//!   "features": [], "file_type": "tar.gz", "image_type": "jdk",
//!   "java_version": "11.0.11.9.1", "jvm_impl": "hotspot",
//!   "url": "https://aka.ms/download-jdk/microsoft-jdk-11.0.11.9.1-linux-x64.tar.gz",
//!   "vendor": "microsoft", "version": "11.0.11.9.1"}]
//! ```
//!
//! Everything here is pure: the adapter fetches, the web layer routes, and
//! both agree on a row's coordinate, its `(host, path)` and its checksum
//! through these functions.

use serde::{Deserialize, Serialize};

/// The one package a `mise-java` registry serves.
pub const JAVA_PACKAGE: &str = "java";

/// The only metadata host mise reads (`java.rs:381`).
pub const DEFAULT_UPSTREAM: &str = "https://mise-java.jdx.dev";

/// The largest document probed on 2026-11-14 is 3.5 MB (GA, Linux x86_64);
/// twice that and a little is room to grow and a ceiling on what a hostile
/// upstream can make this process hold (RFC 0048 §7.3).
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// The URL segments a document is addressed by. `{type}` is checked against
/// the registry's `release_types`, these two against fixed lists, before any
/// of them reaches an upstream URL (RFC 0048 §7.3). The lists are what mise
/// sends (`java_os`, `java_arch`, `java.rs:813-833`), probed for which answer.
pub const RELEASE_TYPES: &[&str] = &["ga", "ea"];
pub const OSES: &[&str] = &["linux", "macosx", "windows", "alpine-linux"];
pub const ARCHES: &[&str] = &[
    "x86_64",
    "aarch64",
    "i686",
    "arm32-vfp-hflt",
    "ppc64le",
    "s390x",
    "riscv64",
];

/// Where a download may come from, derived from every GA and EA document
/// probed on 2026-11-14 (62 212 rows): host + path prefix, GitHub by owner.
pub const DEFAULT_VENDOR_HOSTS: &[&str] = &[
    "aka.ms/download-jdk/",
    "cache-redirector.jetbrains.com/intellij-jbr/",
    "cdn.azul.com/zulu/",
    "cdn.azul.com/zulu-embedded/",
    "corretto.aws/downloads/",
    "d3pxv6yz143wms.cloudfront.net/",
    "developers.redhat.com/content-gateway/",
    "download.bell-sw.com/vm/",
    "download.java.net/java/",
    "download.oracle.com/graalvm/",
    "download.oracle.com/java/",
    "gds.oracle.com/download/",
    "github.com/SAP/",
    "github.com/Tencent/",
    "github.com/TravaOpenJDK/",
    "github.com/adoptium/",
    "github.com/bell-sw/",
    "github.com/dragonwell-project/",
    "github.com/graalvm/",
    "github.com/ibmruntimes/",
];

/// The hosts the vendor hosts above redirect to, measured on 2026-11-14 by
/// following one row of every prefix.
pub const DEFAULT_REDIRECT_HOSTS: &[&str] = &[
    // aka.ms → 301
    "download.visualstudio.microsoft.com",
    // cache-redirector.jetbrains.com → 307
    "d2xrhe97vsfxuc.cloudfront.net",
    // gds.oracle.com → 302
    "objectstorage.uk-london-1.oraclecloud.com",
    // github.com → 302, every release asset
    "release-assets.githubusercontent.com",
    // developers.redhat.com → 302
    "access.cdn.redhat.com",
];

/// Hosts shared by many unrelated publishers: a bare entry would admit any of
/// them, so `vendor_hosts` must name a path prefix on these (RFC 0048 §15 Q8).
pub const MULTI_TENANT_HOSTS: &[&str] = &[
    "github.com",
    "gitlab.com",
    "codeberg.org",
    "bitbucket.org",
    "sourceforge.net",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "raw.githubusercontent.com",
    "storage.googleapis.com",
    "s3.amazonaws.com",
];

/// One row of a metadata document: the fields mise reads (`JavaMetadata`,
/// `java.rs:861-877`), and nothing else — a field this proxy does not know
/// never reaches a client.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Row {
    pub checksum: Option<String>,
    pub created_at: Option<String>,
    pub features: Option<Vec<String>>,
    pub file_type: Option<String>,
    pub image_type: Option<String>,
    pub java_version: String,
    pub jvm_impl: String,
    pub url: String,
    pub vendor: String,
    pub version: String,
}

/// Parse a metadata document. `None` over [`MAX_DOCUMENT_BYTES`] or not an
/// array of rows.
pub fn parse_document(body: &[u8]) -> Option<Vec<Row>> {
    if body.len() > MAX_DOCUMENT_BYTES {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// One document's address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Platform {
    pub release_type: String,
    pub os: String,
    pub arch: String,
}

impl Platform {
    /// `{type}/{os}/{arch}` checked against the fixed lists; `None` otherwise.
    pub fn parse(release_type: &str, os: &str, arch: &str) -> Option<Self> {
        (RELEASE_TYPES.contains(&release_type) && OSES.contains(&os) && ARCHES.contains(&arch))
            .then(|| Self {
                release_type: release_type.to_owned(),
                os: os.to_owned(),
                arch: arch.to_owned(),
            })
    }

    /// The listing package a document is cached and filtered under:
    /// `java/{type}/{os}/{arch}`. A block is a statement about `java`
    /// ([`package_of`]); the document is one of many.
    pub fn listing_name(&self) -> String {
        format!(
            "{JAVA_PACKAGE}/{}/{}/{}",
            self.release_type, self.os, self.arch
        )
    }

    /// The inverse of [`Self::listing_name`].
    pub fn from_listing_name(name: &str) -> Option<Self> {
        let mut it = name.split('/');
        let (Some(JAVA_PACKAGE), Some(t), Some(os), Some(arch), None) =
            (it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            return None;
        };
        Self::parse(t, os, arch)
    }

    /// The upstream path: `jvm/{type}/{os}/{arch}.json`.
    pub fn path(&self) -> String {
        format!("jvm/{}/{}/{}.json", self.release_type, self.os, self.arch)
    }
}

/// `java/ga/linux/x86_64` → `java`: the package a listing's blocks are read
/// from.
pub fn package_of(name: &str) -> &str {
    name.split('/').next().unwrap_or(name)
}

/// The features mise puts in a version key (`JAVA_FEATURES`, `java.rs:946-958`),
/// **less `musl`**. mise drops `musl` on a musl host and keeps it elsewhere, so
/// one build has two keys depending on who asks; here a release is one
/// coordinate whatever the libc, and a block covers both builds.
const KEY_FEATURES: &[&str] = &["crac", "innovation", "javafx", "jcef", "leyden", "lite"];

/// A row's coordinate: the version key mise builds for it
/// (`to_version_string`, `java.rs:911-944`) — `temurin-21.0.5+11.0.LTS`,
/// `temurin-jre-21.0.5+11.0.LTS`, `liberica-nik-openjdk21-23.1.5+1` —
/// except that `musl` is never part of it ([`KEY_FEATURES`]).
pub fn coordinate(row: &Row) -> String {
    let mut parts = vec![row.vendor.as_str()];
    match row.image_type.as_deref() {
        Some("jre") => parts.push("jre"),
        None => parts.push("unknown"),
        Some(_) => {}
    }
    for f in row.features.iter().flatten() {
        if KEY_FEATURES.contains(&f.as_str()) {
            parts.push(f);
        }
    }
    if row.jvm_impl == "openj9" {
        parts.push("openj9");
    }
    let nik;
    if row.vendor == "liberica-nik" {
        let major = row.java_version.split('.').next().unwrap_or("");
        nik = format!("openjdk{major}");
        parts.push(&nik);
    }
    parts.push(&row.version);
    parts.join("-")
}

/// `https://cdn.azul.com/zulu/bin/x.tar.gz` → `cdn.azul.com/zulu/bin/x.tar.gz`,
/// as the row spells it (percent-escapes kept). `None` for anything that is not
/// an `https://` URL with a host and a path, or that carries a query, a
/// fragment, a port, userinfo, a `..` segment or a doubled `/` — a row the
/// download route could never address the same way twice.
pub fn host_path(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    let path_ok = !path.is_empty()
        && path.bytes().all(|b| b.is_ascii_graphic())
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains(['?', '#', '\\'])
        && !path.contains("//")
        && !path.split('/').any(|s| s == ".." || s == ".");
    (host_ok && path_ok).then_some(rest)
}

/// What a `(host, path)` is compared as: percent-decoded once, the host
/// lowercased. A client's rewritten URL and the row may spell `+` as `%2B` or
/// not; both reach the same key.
pub fn index_key(host_path: &str) -> String {
    let decoded = percent_decode(host_path);
    match decoded.split_once('/') {
        Some((host, path)) => format!("{}/{path}", host.to_ascii_lowercase()),
        None => decoded.to_ascii_lowercase(),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `host_path` (a [`index_key`]) is under one of `vendor_hosts`: an
/// entry with a path is a prefix ending in `/`, a bare entry is the host.
pub fn vendor_allows(key: &str, vendor_hosts: &[String]) -> bool {
    vendor_hosts.iter().any(|entry| {
        let entry = entry.to_ascii_lowercase();
        if entry.contains('/') {
            key.starts_with(&entry)
        } else {
            key.split_once('/').is_some_and(|(h, _)| h == entry)
        }
    })
}

/// How well a row's checksum lets the cache write verify its archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumClass {
    /// SHA-256 or SHA-512.
    Strong,
    /// SHA-1: verified, with a broken algorithm.
    Sha1,
    /// MD5: kept, cached unverified — `integrity` has no MD5.
    Md5,
    /// No checksum at all, or one in no known form.
    None,
}

impl ChecksumClass {
    /// The `checksum` label of `batlehub_mise_java_weak_downloads_total`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Sha1 => "sha1",
            Self::Md5 => "md5",
            Self::None => "none",
        }
    }
}

/// mise's `algo:hex` → the bare hex `integrity::parse_expected` reads, and
/// what the cache write can do with it (RFC 0048 §6.3, T5). Every row is kept
/// whatever its class (§15 Q10).
pub fn normalise_checksum(checksum: Option<&str>) -> (Option<String>, ChecksumClass) {
    let Some((algo, hex)) = checksum.and_then(|c| c.split_once(':')) else {
        return (None, ChecksumClass::None);
    };
    let hex = hex.trim().to_ascii_lowercase();
    let len_ok = |n: usize| hex.len() == n && hex.bytes().all(|b| b.is_ascii_hexdigit());
    match algo.trim().to_ascii_lowercase().as_str() {
        "sha256" if len_ok(64) => (Some(hex), ChecksumClass::Strong),
        "sha512" if len_ok(128) => (Some(hex), ChecksumClass::Strong),
        "sha1" if len_ok(40) => (Some(hex), ChecksumClass::Sha1),
        "md5" => (None, ChecksumClass::Md5),
        _ => (None, ChecksumClass::None),
    }
}

/// Why a row left a served document — the `reason` label of
/// `batlehub_mise_java_rows_dropped_total`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    /// Its URL is not under `vendor_hosts` (R4).
    pub host: usize,
    /// Its URL is not one the download route can address ([`host_path`]).
    pub invalid: usize,
}

/// The rows a registry serves out of an upstream document: those whose URL
/// is addressable and under `vendor_hosts`. Blocked and too-young rows are
/// removed later, by the listing filter, from what this keeps.
pub fn filter_document(rows: Vec<Row>, vendor_hosts: &[String]) -> (Vec<Row>, Dropped) {
    let mut dropped = Dropped::default();
    let kept = rows
        .into_iter()
        .filter(|r| match host_path(&r.url) {
            None => {
                dropped.invalid += 1;
                false
            }
            Some(hp) if !vendor_allows(&index_key(hp), vendor_hosts) => {
                dropped.host += 1;
                false
            }
            Some(_) => true,
        })
        .collect();
    (kept, dropped)
}

/// A row's `created_at` (`2025-03-28T22:04:32.217430`, naive, UTC) — what the
/// age gate reads.
pub fn created_at(row: &Row) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = row.created_at.as_deref()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|n| n.and_utc())
        })
}

/// The coordinates of a served document's rows younger than the gate (and
/// the undated ones when it denies a missing timestamp) — what the age gate
/// removes from the listing, since a version that is listed and then refused
/// at download makes mise fail later and louder.
pub fn younger_than(
    doc: &serde_json::Value,
    gate: crate::rules::ListingAgeGate,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let min_age = chrono::Duration::from_std(gate.min_age).unwrap_or(chrono::Duration::MAX);
    doc.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value::<Row>(v.clone()).ok())
        .filter(|r| match created_at(r) {
            Some(t) => now - t < min_age,
            None => gate.deny_missing_timestamp,
        })
        .map(|r| coordinate(&r))
        .collect()
}

/// The served document's metrics (RFC 0048 §10.1): its row count after
/// filtering, and the rows the block list and the age gate removed.
pub fn record_served(
    registry: &str,
    listing: &str,
    doc: &crate::ports::VersionDocument,
    removed: usize,
    too_young: usize,
) {
    if let Some(p) = Platform::from_listing_name(listing) {
        let rows = doc
            .body
            .as_json()
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        metrics::gauge!("batlehub_mise_java_document_rows",
            "registry" => registry.to_owned(), "type" => p.release_type,
            "os" => p.os, "arch" => p.arch)
        .set(rows as f64);
    }
    count_dropped(registry, "blocked", removed.saturating_sub(too_young));
    count_dropped(registry, "age", too_young);
}

/// `batlehub_mise_java_rows_dropped_total{registry, reason}`.
pub fn count_dropped(registry: &str, reason: &'static str, n: usize) {
    if n > 0 {
        metrics::counter!("batlehub_mise_java_rows_dropped_total",
            "registry" => registry.to_owned(), "reason" => reason)
        .increment(n as u64);
    }
}

/// Remove the rows whose coordinate is blocked; the coordinates removed.
pub fn strip_blocked(doc: &mut serde_json::Value, blocked: impl Fn(&str) -> bool) -> Vec<String> {
    let Some(rows) = doc.as_array_mut() else {
        tracing::warn!("a mise-java document is not an array; passing through unfiltered");
        return Vec::new();
    };
    let mut removed = Vec::new();
    rows.retain(|v| {
        let Ok(row) = serde_json::from_value::<Row>(v.clone()) else {
            return true;
        };
        let c = coordinate(&row);
        if blocked(&c) {
            removed.push(c);
            false
        } else {
            true
        }
    });
    removed
}

/// The order in which the platform documents are searched for a download
/// URL nobody has resolved yet — a lock-driven install asks for the archive
/// without ever reading a document. Hints in the file name come first, so the
/// usual cost is one document; every one is searched before the answer is
/// `404`.
pub fn search_order(host_path: &str, release_types: &[String]) -> Vec<Platform> {
    let file = host_path
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| file.contains(w));
    let os_score = |os: &str| match os {
        "macosx" => has(&["mac", "osx", "darwin"]),
        "windows" => has(&["win"]),
        "alpine-linux" => has(&["alpine", "musl"]),
        _ => has(&["linux"]) && !has(&["alpine", "musl"]),
    };
    let arch_score = |arch: &str| match arch {
        "x86_64" => has(&["x64", "x86_64", "amd64"]),
        "aarch64" => has(&["aarch64", "arm64"]),
        "i686" => has(&["i686", "x86-32", "x86_32", "i586", "i386"]),
        _ => has(&[arch]),
    };
    let mut all: Vec<(u8, Platform)> = Vec::new();
    for t in release_types {
        for os in OSES {
            for arch in ARCHES {
                if let Some(p) = Platform::parse(t, os, arch) {
                    let score = u8::from(!os_score(os)) + u8::from(!arch_score(arch));
                    all.push((score, p));
                }
            }
        }
    }
    all.sort_by_key(|(s, _)| *s);
    all.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(url: &str, checksum: Option<&str>) -> Row {
        Row {
            checksum: checksum.map(str::to_owned),
            created_at: Some("2025-03-28T22:04:32.217430".into()),
            features: Some(vec![]),
            file_type: Some("tar.gz".into()),
            image_type: Some("jdk".into()),
            java_version: "21.0.5+11".into(),
            jvm_impl: "hotspot".into(),
            url: url.into(),
            vendor: "temurin".into(),
            version: "21.0.5+11.0.LTS".into(),
        }
    }

    #[test]
    fn the_coordinate_is_the_key_mise_builds_without_musl() {
        let mut r = row("https://x/y", None);
        assert_eq!(coordinate(&r), "temurin-21.0.5+11.0.LTS");
        r.image_type = Some("jre".into());
        r.features = Some(vec!["musl".into(), "javafx".into(), "extended".into()]);
        assert_eq!(coordinate(&r), "temurin-jre-javafx-21.0.5+11.0.LTS");
        r.image_type = None;
        r.features = None;
        r.jvm_impl = "openj9".into();
        assert_eq!(coordinate(&r), "temurin-unknown-openj9-21.0.5+11.0.LTS");
        let nik = Row {
            vendor: "liberica-nik".into(),
            java_version: "21.0.5".into(),
            version: "23.1.5+1".into(),
            image_type: Some("jdk".into()),
            ..Row::default()
        };
        assert_eq!(coordinate(&nik), "liberica-nik-openjdk21-23.1.5+1");
    }

    #[test]
    fn checksums_normalise_to_bare_hex_and_a_class() {
        let h64 = "AB".repeat(32);
        assert_eq!(
            normalise_checksum(Some(&format!("sha256:{h64}"))),
            (Some("ab".repeat(32)), ChecksumClass::Strong)
        );
        assert_eq!(
            normalise_checksum(Some(&format!("sha512:{}", "c".repeat(128)))).1,
            ChecksumClass::Strong
        );
        assert_eq!(
            normalise_checksum(Some(&format!("sha1:{}", "d".repeat(40)))),
            (Some("d".repeat(40)), ChecksumClass::Sha1)
        );
        assert_eq!(
            normalise_checksum(Some("md5:0123")),
            (None, ChecksumClass::Md5)
        );
        assert_eq!(normalise_checksum(None), (None, ChecksumClass::None));
        assert_eq!(normalise_checksum(Some("")), (None, ChecksumClass::None));
        assert_eq!(
            normalise_checksum(Some("sha256:short")),
            (None, ChecksumClass::None)
        );
    }

    #[test]
    fn host_paths_are_https_and_addressable() {
        assert_eq!(
            host_path("https://cdn.azul.com/zulu/bin/a.tar.gz"),
            Some("cdn.azul.com/zulu/bin/a.tar.gz")
        );
        for bad in [
            "http://cdn.azul.com/zulu/a",
            "https://cdn.azul.com",
            "https://cdn.azul.com/zulu/../x",
            "https://cdn.azul.com/a?b",
            "https://cdn.azul.com:8443/a",
            "https://u@cdn.azul.com/a",
            "https://cdn.azul.com//a",
        ] {
            assert_eq!(host_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_plus_and_its_escape_are_one_key() {
        assert_eq!(
            index_key("GitHub.com/SAP/SapMachine/releases/download/sapmachine-10%2B46-2/x"),
            "github.com/SAP/SapMachine/releases/download/sapmachine-10+46-2/x"
        );
        assert_eq!(index_key("a.b/c%2"), "a.b/c%2");
    }

    #[test]
    fn vendor_hosts_are_prefixes_or_hosts() {
        let hosts = vec!["github.com/adoptium/".to_owned(), "aka.ms".to_owned()];
        assert!(vendor_allows("github.com/adoptium/t/x", &hosts));
        assert!(!vendor_allows("github.com/adoptiumx/t/x", &hosts));
        assert!(!vendor_allows("github.com/evil/t/x", &hosts));
        assert!(vendor_allows("aka.ms/download-jdk/x", &hosts));
        assert!(!vendor_allows("aka.ms.evil.example/x", &hosts));
    }

    #[test]
    fn filter_drops_off_list_and_unaddressable_rows() {
        let hosts: Vec<String> = DEFAULT_VENDOR_HOSTS.iter().map(|s| s.to_string()).collect();
        let rows = vec![
            row(
                "https://github.com/adoptium/temurin21-binaries/x.tar.gz",
                None,
            ),
            row("https://github.com/someone/jdk/x.tar.gz", None),
            row("http://cdn.azul.com/zulu/x.tar.gz", None),
            row("https://169.254.169.254/latest/meta-data", None),
        ];
        let (kept, dropped) = filter_document(rows, &hosts);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            dropped,
            Dropped {
                host: 2,
                invalid: 1
            }
        );
    }

    #[test]
    fn the_age_gate_and_the_block_list_read_the_coordinate() {
        let mut doc = serde_json::to_value(vec![
            row("https://a/b", None),
            Row {
                version: "22".into(),
                created_at: None,
                ..row("https://a/c", None)
            },
        ])
        .unwrap();
        let now = chrono::Utc::now();
        let gate = |deny_missing_timestamp| crate::rules::ListingAgeGate {
            min_age: std::time::Duration::from_secs(60),
            deny_missing_timestamp,
        };
        assert!(younger_than(&doc, gate(false), now).is_empty());
        let young = younger_than(&doc, gate(true), now);
        assert_eq!(young, vec!["temurin-22".to_owned()]);
        let removed = strip_blocked(&mut doc, |c| c == "temurin-21.0.5+11.0.LTS");
        assert_eq!(removed, vec!["temurin-21.0.5+11.0.LTS".to_owned()]);
        assert_eq!(doc.as_array().unwrap().len(), 1);
    }

    #[test]
    fn platforms_are_checked_and_searched_by_hint() {
        assert!(Platform::parse("ga", "linux", "x86_64").is_some());
        assert!(Platform::parse("nightly", "linux", "x86_64").is_none());
        assert!(Platform::parse("ga", "../x", "x86_64").is_none());
        let p = Platform::from_listing_name("java/ga/macosx/aarch64").unwrap();
        assert_eq!(p.path(), "jvm/ga/macosx/aarch64.json");
        assert_eq!(Platform::from_listing_name(&p.listing_name()), Some(p));
        assert_eq!(Platform::from_listing_name("java/ga/macosx"), None);
        let order = search_order(
            "cdn.azul.com/zulu/bin/zulu21-macosx_aarch64.tar.gz",
            &["ga".to_owned()],
        );
        assert_eq!(order[0].listing_name(), "java/ga/macosx/aarch64");
        assert_eq!(order.len(), OSES.len() * ARCHES.len());
    }

    #[test]
    fn the_document_parses_and_is_capped() {
        let body = serde_json::to_vec(&vec![row("https://a/b", None)]).unwrap();
        assert_eq!(parse_document(&body).unwrap().len(), 1);
        assert!(parse_document(b"{}").is_none());
        assert!(parse_document(&vec![b' '; MAX_DOCUMENT_BYTES + 1]).is_none());
    }
}
