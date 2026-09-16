//! The Rust toolchain tree's vocabulary, read once for the places that need it.
//!
//! RFC 0024 §6.2. `static.rust-lang.org/dist` has no metadata API: it has one
//! channel manifest per release, one directory of tarballs per date, and
//! `manifests.txt` listing every manifest ever published. Four readers need to
//! agree about that tree — the handler that answers a request, the filter that
//! hides a blocked release from `manifests.txt`, the repair walk that moves an
//! alias off a blocked release, and the client that reads a release date for
//! the age gate — so the grammar lives here and they ask it questions.
//!
//! Nothing in this module does I/O, and nothing here parses TOML. A channel
//! manifest is 900 KB of `[section]` blocks; the read path needs four fields
//! and a list of headers out of it, and re-serialising a value tree would end
//! the byte-exact property the `.asc` and the sidecar depend on (§6.2). So
//! [`Manifest`] is a *section index* over the original text, and every edit is
//! a whole-block replacement.

use std::borrow::Cow;

use chrono::NaiveDate;

/// The one package a `rustup` registry serves toolchains under.
///
/// The thing an admin blocks is *a Rust release*, and
/// `rustup toolchain install <name>` is the sentence a block has to defeat, so
/// the version is spelled the way that sentence spells it (RFC 0024 §4.3).
pub const RUST_PACKAGE: &str = "rust";

/// The second package: the installer's own self-update tree, whose versions are
/// rustup's releases (`1.29.1`) and never a moving name.
pub const RUSTUP_PACKAGE: &str = "rustup";

/// How far back an alias repair walks for a dated channel.
///
/// rustup's own `RUSTUP_BACKTRACK_LIMIT` when it hunts for a nightly with a
/// missing component, so a client and this proxy give up at the same horizon
/// (RFC 0024 §4.4).
pub const BACKTRACK_DAYS: i64 = 21;

/// The package a listing package string names.
///
/// `rust/stable` → `rust`; `rust/2026-09-05/nightly` → `rust`; `rustup` →
/// `rustup`. A channel manifest describes one release and every channel needs
/// its own cache entry, so the channel travels in the listing package string
/// the way SDKMAN's platform does — and a block is still a statement about the
/// release, held on `rust` (RFC 0024 §6.2).
pub fn package_of(package: &str) -> &str {
    let end = package.find('/').unwrap_or(package.len());
    &package[..end]
}

/// The listing package string a channel's manifest is addressed by.
pub fn listing_package(channel: &str) -> String {
    format!("{RUST_PACKAGE}/{channel}")
}

/// The channel part of a listing package string, or `None` when there is none.
pub fn channel_of(package: &str) -> Option<&str> {
    package.split_once('/').map(|(_, channel)| channel)
}

/// What a toolchain name denotes, before any date is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
    /// A version as the name spells it: `1.98.1`, `1.98`, `1.99.0-beta`,
    /// `1.99.0-beta.5`.
    Version(String),
}

/// A toolchain name, as rustup spells one.
///
/// The grammar is rustup 1.29's `ToolchainDesc` regex, minus the trailing
/// target triple this proxy never sees in a manifest path:
///
/// ```text
/// (?:nightly|beta|stable|\d{1,3}\.\d{1,3}(?:\.\d{1,3})?(?:-beta(?:\.\d{1,2})?)?)(?:-(\d{4}-\d{2}-\d{2}))?
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainName {
    pub channel: Channel,
    /// `2026-09-05`, when the name carries one.
    pub date: Option<String>,
}

impl ToolchainName {
    /// Parse a name, or say why it is not one.
    ///
    /// Called at the edge on every manifest request, because the value becomes
    /// part of a cache key: anything this rejects is a `400` rather than a
    /// lookup for a name no release could have.
    pub fn parse(s: &str) -> Result<Self, String> {
        let reject = || format!("'{s}' is not a rustup toolchain name");
        if s.is_empty() || s.len() > 64 {
            return Err(reject());
        }
        // A date is itself three dash-separated fields, so it is found by
        // looking back three dashes rather than by splitting on the last one.
        let (head, date) = match split_trailing_date(s) {
            Some((head, date)) => (head, Some(date.to_owned())),
            None => (s, None),
        };
        let channel = match head {
            "stable" => Channel::Stable,
            "beta" => Channel::Beta,
            "nightly" => Channel::Nightly,
            other if is_version_name(other) => Channel::Version(other.to_owned()),
            _ => return Err(reject()),
        };
        Ok(Self { channel, date })
    }

    /// Whether this name denotes exactly one release, for as long as the tree
    /// exists.
    ///
    /// A dated name always does. Undated, only a full `x.y.z` — or a numbered
    /// beta, `1.99.0-beta.5` — does: `stable`, `beta`, `nightly`, `1.98` and
    /// `1.99.0-beta` all move (RFC 0024 §4.4).
    pub fn is_exact(&self) -> bool {
        if self.date.is_some() {
            return true;
        }
        match &self.channel {
            Channel::Stable | Channel::Beta | Channel::Nightly => false,
            Channel::Version(v) => match v.split_once("-beta") {
                // `1.99.0-beta.5` is one release; `1.99.0-beta` is whichever
                // beta is current.
                Some((_, suffix)) => suffix.starts_with('.') && suffix.len() > 1,
                None => v.split('.').count() == 3,
            },
        }
    }

    /// The name as a manifest file spells it: `channel-rust-{this}.toml`.
    pub fn as_name(&self) -> String {
        let base = match &self.channel {
            Channel::Stable => "stable".to_owned(),
            Channel::Beta => "beta".to_owned(),
            Channel::Nightly => "nightly".to_owned(),
            Channel::Version(v) => v.clone(),
        };
        match &self.date {
            Some(d) => format!("{base}-{d}"),
            None => base,
        }
    }
}

/// `2026-09-05`.
fn is_date(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// Split a trailing `-YYYY-MM-DD` off a name: `nightly-2026-09-05` →
/// `("nightly", "2026-09-05")`.
fn split_trailing_date(s: &str) -> Option<(&str, &str)> {
    let mut idx = s.len();
    for _ in 0..3 {
        idx = s[..idx].rfind('-')?;
    }
    let candidate = &s[idx + 1..];
    is_date(candidate).then(|| (&s[..idx], candidate))
}

/// `1.98`, `1.98.1`, `1.99.0-beta`, `1.99.0-beta.5`.
fn is_version_name(s: &str) -> bool {
    let (release, beta) = match s.split_once("-beta") {
        Some((r, b)) => (r, Some(b)),
        None => (s, None),
    };
    if let Some(b) = beta {
        // Either nothing, or `.N`.
        if !(b.is_empty()
            || (b.starts_with('.') && b[1..].chars().all(|c| c.is_ascii_digit()) && b.len() > 1))
        {
            return false;
        }
    }
    let parts: Vec<&str> = release.split('.').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
}

/// A component archive under a dated directory, split into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFile {
    /// `rust-std`, `rustc`, `llvm-tools-preview`.
    pub component: String,
    /// The target triple, absent for the target-independent components
    /// (`rust-src`).
    pub target: Option<String>,
    /// Everything after the stem: `.tar.xz`, `.tar.gz.sha256`, `.tar.xz.asc`.
    pub ext: String,
}

/// The extensions the dist tree serves, longest first so `.tar.xz.sha256` is
/// not read as `.tar.xz` plus a component called `sha256`.
const EXTENSIONS: &[&str] = &[
    ".tar.gz.sha256",
    ".tar.xz.sha256",
    ".tar.zst.sha256",
    ".tar.gz.asc",
    ".tar.xz.asc",
    ".tar.zst.asc",
    ".tar.gz",
    ".tar.xz",
    ".tar.zst",
    ".msi",
    ".pkg",
    ".exe",
];

/// The coordinate a dated file belongs to: `(version, parts)`.
///
/// The mapping is read off the file name and the directory, with no manifest
/// fetch (RFC 0024 §4.3). A component name can contain dashes (`rust-std`) and
/// so can a target triple, so the walk takes the **first** dash-separated token
/// that is `nightly`, `beta` or shaped `x.y.z`: what precedes it is the
/// component, what follows is the target.
///
/// ```text
/// ("2026-09-03", "rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz")
///     → ("1.98.1", rust-std / x86_64-unknown-linux-gnu / .tar.xz)
/// ("2026-09-05", "rustc-nightly-aarch64-apple-darwin.tar.xz")
///     → ("nightly-2026-09-05", rustc / aarch64-apple-darwin / .tar.xz)
/// ("2026-09-11", "rust-src-beta.tar.xz")
///     → ("beta-2026-09-11", rust-src / none / .tar.xz)
/// ```
pub fn coordinate_of(date: &str, file: &str) -> Result<(String, ComponentFile), String> {
    if !is_date(date) {
        return Err(format!("'{date}' is not a dist directory date"));
    }
    if file.contains('/') || file.contains('\\') || file.contains("..") || file.is_empty() {
        return Err(format!("'{file}' is not a dist file name"));
    }
    let Some(ext) = EXTENSIONS.iter().find(|e| file.ends_with(**e)) else {
        return Err(format!("'{file}' has no dist file extension"));
    };
    let stem = &file[..file.len() - ext.len()];

    let tokens: Vec<&str> = stem.split('-').collect();
    let at = tokens
        .iter()
        .position(|t| *t == "nightly" || *t == "beta" || is_full_version(t))
        .ok_or_else(|| format!("'{file}' names no Rust version"))?;
    if at == 0 {
        return Err(format!("'{file}' names no component"));
    }
    let component = tokens[..at].join("-");
    let target = if at + 1 < tokens.len() {
        Some(tokens[at + 1..].join("-"))
    } else {
        None
    };
    let version = match tokens[at] {
        "nightly" => format!("nightly-{date}"),
        "beta" => format!("beta-{date}"),
        v => v.to_owned(),
    };
    Ok((
        version,
        ComponentFile {
            component,
            target,
            ext: (*ext).to_owned(),
        },
    ))
}

/// `1.98.1` — a full three-part version, the only shape a *file name* carries.
fn is_full_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// One `[header]` block of a channel manifest, as a byte range over the
/// original text.
#[derive(Debug, Clone, Copy)]
struct Section<'a> {
    header: &'a str,
    /// The whole block including its header line and trailing blank line.
    start: usize,
    end: usize,
    /// The body, after the header line.
    body_start: usize,
}

/// A channel manifest, indexed by section rather than parsed.
#[derive(Debug, Clone)]
pub struct Manifest<'a> {
    text: &'a str,
    date: Option<&'a str>,
    rust_version: Option<&'a str>,
    sections: Vec<Section<'a>>,
}

impl<'a> Manifest<'a> {
    /// One pass over the lines. Never fails: a document this does not
    /// understand yields no sections and no fields, which every caller treats
    /// as "pass it through".
    pub fn parse(text: &'a str) -> Self {
        let mut sections: Vec<Section<'a>> = Vec::new();
        let mut date = None;
        let mut rust_version = None;
        let mut offset = 0usize;
        let mut in_pkg_rust = false;

        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            let start = offset;
            offset += line.len();

            if trimmed.starts_with('[') {
                if let Some(prev) = sections.last_mut() {
                    prev.end = start;
                }
                let header = trimmed
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim()
                    .to_owned();
                // The header is borrowed from `text` so the index stays a view:
                // find it inside the line rather than owning the trimmed copy.
                let header = trimmed
                    .find(&header)
                    .map(|i| &trimmed[i..i + header.len()])
                    .unwrap_or(trimmed);
                in_pkg_rust = header == "pkg.rust";
                sections.push(Section {
                    header,
                    start,
                    end: text.len(),
                    body_start: offset,
                });
                continue;
            }

            if sections.is_empty() {
                // The preamble: `manifest-version` and `date`.
                if let Some(v) = toml_string_value(trimmed, "date") {
                    date = Some(v);
                }
            } else if in_pkg_rust && rust_version.is_none() {
                // `version = "1.98.1 (48a229cea 2026-09-01)"` — the release's
                // own version is the first token.
                if let Some(v) = toml_string_value(trimmed, "version") {
                    rust_version = Some(v.split_whitespace().next().unwrap_or(v));
                }
            }
        }
        Self {
            text,
            date,
            rust_version,
            sections,
        }
    }

    /// The manifest's own `date`.
    pub fn date(&self) -> Option<&'a str> {
        self.date
    }

    /// `[pkg.rust] version`, first token: `1.98.1`, or `1.99.0-nightly`.
    pub fn rust_version(&self) -> Option<&'a str> {
        self.rust_version
    }

    /// The coordinate this document describes, in the spelling a block uses.
    ///
    /// A release manifest names its own channel in `[pkg.rust] version`: a
    /// stable release is `1.98.1`, and a pre-release carries the channel as a
    /// suffix, which the dated coordinate replaces.
    pub fn coordinate(&self) -> Option<String> {
        let version = self.rust_version?;
        let date = self.date;
        if version.ends_with("-nightly") {
            return date.map(|d| format!("nightly-{d}"));
        }
        if version.contains("-beta") {
            return date.map(|d| format!("beta-{d}"));
        }
        Some(version.to_owned())
    }

    /// Every section header, in document order. For tests and for warming.
    pub fn headers(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.sections.iter().map(|s| s.header)
    }

    /// The body text of the first section with this header.
    pub fn section_body(&self, header: &str) -> Option<&'a str> {
        self.sections
            .iter()
            .find(|s| s.header == header)
            .map(|s| &self.text[s.body_start..s.end])
    }
}

/// `key = "value"` → `value`, borrowed.
fn toml_string_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(&inner[..end])
}

/// Serve a manifest with every denied component edited out.
///
/// Returns [`Cow::Borrowed`] when nothing applies, so the `upstream` case is a
/// pointer rather than a copy. The edit is textual and section-aware, exactly
/// as upstream edits a component that failed to build (RFC 0024 §4.4):
///
/// - every `[[pkg.rust.target.{t}.components]]` / `…extensions]]` block whose
///   body names a denied `pkg` is **dropped**, which is how `rust-mingw` sits
///   in every profile and installs on no Linux host;
/// - every `[pkg.{denied}.target.{t}]` block's body becomes `available = false`,
///   the state hundreds of tables in today's stable manifest are already in.
///
/// The dropped list entry is the edit the client answers from: `rustup
/// component add <denied>` stops on rustup's own *"toolchain '…' does not
/// contain component '…' for target '…'"* without a request, which
/// `tests/heavy/rustup.sh` measures. `available = false` alone would produce
/// *"unavailable for download"* instead, one round trip later.
///
/// `[profiles]` and `[renames]` are left alone: both tolerate a name the
/// targets do not carry. The failure mode of an upstream format change is
/// therefore a component this failed to remove, never a document rustup cannot
/// parse.
pub fn render_manifest<'a>(body: &'a str, denied: &[String]) -> Cow<'a, str> {
    if denied.is_empty() {
        return Cow::Borrowed(body);
    }
    let manifest = Manifest::parse(body);
    if manifest.sections.is_empty() {
        return Cow::Borrowed(body);
    }

    let mut out = String::with_capacity(body.len());
    let mut cursor = 0usize;
    let mut edited = false;

    for section in &manifest.sections {
        let block = &body[section.start..section.end];
        let header = section.header;

        // `[[pkg.rust.target.{t}.components]]` and `…extensions]]`: the `pkg`
        // is in the body, so the whole block is read to decide.
        if header.ends_with(".components") || header.ends_with(".extensions") {
            let pkg = toml_string_value_in(&body[section.body_start..section.end], "pkg");
            if pkg.is_some_and(|p| denied.iter().any(|d| d == p)) {
                out.push_str(&body[cursor..section.start]);
                cursor = section.end;
                edited = true;
            }
            continue;
        }

        // `[pkg.{name}.target.{t}]`: keep the header, replace the body.
        if let Some(name) = header
            .strip_prefix("pkg.")
            .and_then(|rest| rest.split(".target.").next())
        {
            if denied.iter().any(|d| d == name) && header.contains(".target.") {
                out.push_str(&body[cursor..section.body_start]);
                out.push_str("available = false\n\n");
                cursor = section.end;
                edited = true;
            }
        }
        let _ = block;
    }

    if !edited {
        return Cow::Borrowed(body);
    }
    out.push_str(&body[cursor..]);
    Cow::Owned(out)
}

/// `key = "value"` anywhere in a block.
fn toml_string_value_in<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    block.lines().find_map(|l| toml_string_value(l, key))
}

/// The sidecar line the release tooling writes, over the bytes this instance
/// actually serves.
///
/// `"{sha256 hex}  {file name}\n"` — two spaces, the coreutils format.
/// rustup reads the first 64 characters and stores the first 20 as the
/// toolchain's update-hash (RFC 0024 §4.4).
pub fn sidecar_line(bytes: &[u8], file_name: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{}  {file_name}\n", hex::encode(hasher.finalize()))
}

/// One line of `manifests.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRow {
    /// The date directory the manifest lives in.
    pub date: String,
    /// The name between `channel-rust-` and `.toml`: `nightly`, `beta`,
    /// `1.98.1`, `1.99.0-beta.5`, `stable`.
    pub name: String,
    /// The coordinate this manifest describes, when the line alone says:
    /// `nightly-{date}`, `beta-{date}`, `1.98.1`. A dated `stable` snapshot
    /// names no version, so it has none.
    pub coordinate: Option<String>,
}

/// `manifests.txt`: every manifest the release tooling ever published, oldest
/// first.
///
/// The lines are `static.rust-lang.org/dist/{date}/channel-rust-{name}.toml`,
/// 5 157 of them as of 2026-09-12. Read for two jobs: the version list the
/// console shows, and the repair walk that moves an alias off a blocked
/// release (RFC 0024 §4.4).
#[derive(Debug, Clone, Default)]
pub struct ManifestsTxt {
    pub rows: Vec<ManifestRow>,
}

impl ManifestsTxt {
    pub fn parse(text: &str) -> Self {
        let rows = text.lines().filter_map(parse_manifests_line).collect();
        Self { rows }
    }

    /// Every coordinate the file names, in document order (oldest first) and
    /// deduplicated: a release is listed once per manifest that describes it.
    pub fn versions(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.rows
            .iter()
            .filter_map(|r| r.coordinate.clone())
            .filter(|v| seen.insert(v.clone()))
            .collect()
    }

    /// The newest release an alias could denote that is not blocked.
    ///
    /// - `stable` → the newest `channel-rust-{x.y.z}.toml`; `1.98` → the newest
    ///   `1.98.z`.
    /// - `nightly` / `beta` → the newest dated manifest for that channel,
    ///   walking back at most `horizon_days` from the newest one the file
    ///   names, so a client and this proxy give up together.
    ///
    /// `None` when nothing inside the horizon is allowed, which the caller
    /// turns into the `404` rustup would have got from upstream.
    pub fn newest_allowed(
        &self,
        alias: &ToolchainName,
        is_blocked: &dyn Fn(&str) -> bool,
        horizon_days: i64,
    ) -> Option<ManifestRow> {
        match &alias.channel {
            Channel::Nightly | Channel::Beta => {
                let channel = if matches!(alias.channel, Channel::Nightly) {
                    "nightly"
                } else {
                    "beta"
                };
                let candidates: Vec<&ManifestRow> = self
                    .rows
                    .iter()
                    .filter(|r| r.name == channel)
                    .rev()
                    .collect();
                let newest = candidates.first()?;
                let newest_date = NaiveDate::parse_from_str(&newest.date, "%Y-%m-%d").ok()?;
                candidates
                    .into_iter()
                    .take_while(|r| match NaiveDate::parse_from_str(&r.date, "%Y-%m-%d") {
                        Ok(d) => (newest_date - d).num_days() <= horizon_days,
                        Err(_) => false,
                    })
                    .find(|r| r.coordinate.as_deref().is_some_and(|c| !is_blocked(c)))
                    .cloned()
            }
            // `stable` and a partial version both resolve to a full release.
            Channel::Stable | Channel::Version(_) => {
                let prefix = match &alias.channel {
                    Channel::Version(v) => Some(format!("{v}.")),
                    _ => None,
                };
                self.rows
                    .iter()
                    .rev()
                    .filter(|r| is_full_version(&r.name))
                    .filter(|r| match &prefix {
                        Some(p) => r.name.starts_with(p.as_str()),
                        None => true,
                    })
                    .find(|r| r.coordinate.as_deref().is_some_and(|c| !is_blocked(c)))
                    .cloned()
            }
        }
    }
}

fn parse_manifests_line(line: &str) -> Option<ManifestRow> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let file = line.rsplit('/').next()?;
    let name = file.strip_prefix("channel-rust-")?.strip_suffix(".toml")?;
    // `…/dist/{date}/channel-rust-….toml`
    let rest = line.strip_suffix(file)?.trim_end_matches('/');
    let date = rest.rsplit('/').next()?;
    if !is_date(date) {
        return None;
    }
    let coordinate = match name {
        "nightly" => Some(format!("nightly-{date}")),
        "beta" => Some(format!("beta-{date}")),
        "stable" => None,
        v if v.contains("-beta") => Some(format!("beta-{date}")),
        v if is_full_version(v) => Some(v.to_owned()),
        // `channel-rust-1.98.toml` — a partial version, an alias snapshot.
        _ => None,
    };
    Some(ManifestRow {
        date: date.to_owned(),
        name: name.to_owned(),
        coordinate,
    })
}

/// A release date (`2026-09-03`) as the midnight-UTC instant the age gate
/// compares against.
///
/// The tree publishes a day, not a time, and midnight is its earliest instant:
/// a release is never treated as older than it can be (RFC 0010 §13.1's rule,
/// reused).
pub fn parse_release_date(date: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc())
}

/// The date a version's files live under, when the version alone says.
///
/// `nightly-2026-09-05` → `2026-09-05`. A stable version does not carry its
/// date; the caller reads it from `manifests.txt`.
pub fn date_of_version(version: &str) -> Option<&str> {
    split_trailing_date(version).map(|(_, date)| date)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two packages and one target, trimmed from `channel-rust-stable.toml` as
    /// served on 2026-09-03, with the component and extension tables that make
    /// the filter's job real.
    const MANIFEST: &str = r#"manifest-version = "2"
date = "2026-09-03"

[pkg.cargo]
version = "0.99.0 (797e8a9bc 2026-08-05)"

[pkg.cargo.target.x86_64-unknown-linux-gnu]
available = true
url = "https://static.rust-lang.org/dist/2026-09-03/cargo-1.98.1-x86_64-unknown-linux-gnu.tar.gz"
hash = "aaaa"
xz_url = "https://static.rust-lang.org/dist/2026-09-03/cargo-1.98.1-x86_64-unknown-linux-gnu.tar.xz"
xz_hash = "bbbb"

[pkg.rust-docs]
version = "1.98.1 (48a229cea 2026-09-01)"

[pkg.rust-docs.target.x86_64-unknown-linux-gnu]
available = true
url = "https://static.rust-lang.org/dist/2026-09-03/rust-docs-1.98.1-x86_64-unknown-linux-gnu.tar.gz"
hash = "cccc"

[pkg.rust]
version = "1.98.1 (48a229cea 2026-09-01)"

[pkg.rust.target.x86_64-unknown-linux-gnu]
available = true
url = "https://static.rust-lang.org/dist/2026-09-03/rust-1.98.1-x86_64-unknown-linux-gnu.tar.gz"
hash = "dddd"

[[pkg.rust.target.x86_64-unknown-linux-gnu.components]]
pkg = "rustc"
target = "x86_64-unknown-linux-gnu"

[[pkg.rust.target.x86_64-unknown-linux-gnu.components]]
pkg = "rust-docs"
target = "x86_64-unknown-linux-gnu"

[[pkg.rust.target.x86_64-unknown-linux-gnu.extensions]]
pkg = "rust-docs"
target = "aarch64-apple-darwin"

[profiles]
minimal = ["rustc", "cargo", "rust-std"]
default = ["rustc", "cargo", "rust-std", "rust-docs"]

[renames.clippy]
to = "clippy-preview"
"#;

    const MANIFESTS_TXT: &str = "static.rust-lang.org/dist/2026-08-20/channel-rust-1.98.0.toml\n\
        static.rust-lang.org/dist/2026-09-01/channel-rust-nightly.toml\n\
        static.rust-lang.org/dist/2026-09-03/channel-rust-1.98.1.toml\n\
        static.rust-lang.org/dist/2026-09-03/channel-rust-stable.toml\n\
        static.rust-lang.org/dist/2026-09-04/channel-rust-nightly.toml\n\
        static.rust-lang.org/dist/2026-09-05/channel-rust-nightly.toml\n\
        static.rust-lang.org/dist/2026-09-11/channel-rust-beta.toml\n\
        static.rust-lang.org/dist/2026-09-11/channel-rust-1.99.0-beta.5.toml\n";

    // ── the listing package string ────────────────────────────────────────────

    #[test]
    fn the_channel_travels_in_the_package_string_and_the_block_is_held_on_the_package() {
        assert_eq!(package_of("rust/stable"), "rust");
        assert_eq!(package_of("rust/2026-09-05/nightly"), "rust");
        assert_eq!(package_of("rust"), "rust");
        assert_eq!(package_of("rustup"), "rustup");
        assert_eq!(listing_package("stable"), "rust/stable");
        assert_eq!(channel_of("rust/stable"), Some("stable"));
        assert_eq!(channel_of("rust"), None);
    }

    // ── toolchain names ───────────────────────────────────────────────────────

    #[test]
    fn every_shape_rustup_accepts_parses_and_round_trips() {
        for name in [
            "stable",
            "beta",
            "nightly",
            "nightly-2026-09-05",
            "beta-2026-09-11",
            "stable-2026-09-03",
            "1.98",
            "1.98.1",
            "1.99.0-beta",
            "1.99.0-beta.5",
            "1.99.0-beta.5-2026-09-11",
        ] {
            let parsed = ToolchainName::parse(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(parsed.as_name(), name, "round trip for {name}");
        }
    }

    #[test]
    fn a_name_no_release_could_have_is_refused_at_the_edge() {
        for name in [
            "",
            "latest",
            "1",
            "1.2.3.4",
            "nightly-2026-13-01",
            "../../etc/passwd",
            "stable; rm -rf /",
            "1.98.1-alpha",
        ] {
            assert!(
                ToolchainName::parse(name).is_err(),
                "'{name}' should not parse"
            );
        }
    }

    /// The distinction the whole of §4.4 turns on: an exact name is refused
    /// when its release is blocked, an alias is repaired to another release.
    #[test]
    fn exactness_is_what_decides_between_a_404_and_a_repair() {
        for exact in [
            "1.98.1",
            "1.99.0-beta.5",
            "nightly-2026-09-05",
            "beta-2026-09-11",
            "stable-2026-09-03",
        ] {
            assert!(
                ToolchainName::parse(exact).unwrap().is_exact(),
                "{exact} denotes one release"
            );
        }
        for alias in ["stable", "beta", "nightly", "1.98", "1.99.0-beta"] {
            assert!(
                !ToolchainName::parse(alias).unwrap().is_exact(),
                "{alias} moves"
            );
        }
    }

    // ── dated file names ──────────────────────────────────────────────────────

    #[test]
    fn a_dated_file_names_its_release_without_a_manifest_fetch() {
        let (version, parts) = coordinate_of(
            "2026-09-03",
            "rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz",
        )
        .unwrap();
        assert_eq!(version, "1.98.1");
        assert_eq!(parts.component, "rust-std");
        assert_eq!(parts.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(parts.ext, ".tar.xz");

        let (version, parts) = coordinate_of(
            "2026-09-05",
            "rustc-nightly-aarch64-apple-darwin.tar.xz.sha256",
        )
        .unwrap();
        assert_eq!(version, "nightly-2026-09-05");
        assert_eq!(parts.component, "rustc");
        assert_eq!(parts.target.as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(parts.ext, ".tar.xz.sha256");

        // Target-independent: nothing follows the channel token.
        let (version, parts) = coordinate_of("2026-09-11", "rust-src-beta.tar.gz").unwrap();
        assert_eq!(version, "beta-2026-09-11");
        assert_eq!(parts.component, "rust-src");
        assert_eq!(parts.target, None);

        // A component whose own name has three dashes, and a triple with four.
        let (version, parts) = coordinate_of(
            "2026-09-03",
            "llvm-tools-preview-1.98.1-armv7-unknown-linux-gnueabihf.tar.xz",
        )
        .unwrap();
        assert_eq!(version, "1.98.1");
        assert_eq!(parts.component, "llvm-tools-preview");
        assert_eq!(
            parts.target.as_deref(),
            Some("armv7-unknown-linux-gnueabihf")
        );
    }

    #[test]
    fn a_file_that_names_no_release_is_refused_rather_than_guessed_at() {
        for (date, file) in [
            ("2026-09-03", "rustc.tar.xz"),
            ("2026-09-03", "1.98.1-x86_64-unknown-linux-gnu.tar.xz"),
            ("2026-09-03", "rustc-1.98.1-x86_64.zip"),
            ("2026-09-03", "../../../etc/passwd.tar.xz"),
            ("2026-09-03", "a/b.tar.xz"),
            ("not-a-date", "rustc-1.98.1-x86_64-unknown-linux-gnu.tar.xz"),
        ] {
            assert!(
                coordinate_of(date, file).is_err(),
                "{date}/{file} should be refused"
            );
        }
    }

    // ── the manifest, as a section index ──────────────────────────────────────

    #[test]
    fn the_index_reads_the_four_fields_the_read_path_needs() {
        let m = Manifest::parse(MANIFEST);
        assert_eq!(m.date(), Some("2026-09-03"));
        assert_eq!(m.rust_version(), Some("1.98.1"));
        assert_eq!(m.coordinate().as_deref(), Some("1.98.1"));
        let headers: Vec<&str> = m.headers().collect();
        assert!(headers.contains(&"pkg.rust.target.x86_64-unknown-linux-gnu"));
        assert!(headers.contains(&"profiles"));
        assert_eq!(
            headers
                .iter()
                .filter(|h| **h == "pkg.rust.target.x86_64-unknown-linux-gnu.components")
                .count(),
            2,
            "an array-of-tables header repeats, once per entry"
        );
    }

    #[test]
    fn a_pre_release_manifest_takes_its_coordinate_from_the_date() {
        let nightly = MANIFEST
            .replace("date = \"2026-09-03\"", "date = \"2026-09-05\"")
            .replace(
                "[pkg.rust]\nversion = \"1.98.1 (48a229cea 2026-09-01)\"",
                "[pkg.rust]\nversion = \"1.99.0-nightly (abcdef012 2026-09-04)\"",
            );
        assert_eq!(
            Manifest::parse(&nightly).coordinate().as_deref(),
            Some("nightly-2026-09-05")
        );

        let beta = MANIFEST.replace(
            "[pkg.rust]\nversion = \"1.98.1 (48a229cea 2026-09-01)\"",
            "[pkg.rust]\nversion = \"1.99.0-beta.5 (abcdef012 2026-09-01)\"",
        );
        assert_eq!(
            Manifest::parse(&beta).coordinate().as_deref(),
            Some("beta-2026-09-03")
        );
    }

    #[test]
    fn a_document_this_does_not_understand_yields_nothing_rather_than_a_guess() {
        let m = Manifest::parse("not a manifest at all\n");
        assert!(m.date().is_none());
        assert!(m.coordinate().is_none());
        assert_eq!(m.headers().count(), 0);
    }

    // ── the deny-list render ──────────────────────────────────────────────────

    #[test]
    fn nothing_denied_is_a_pointer_not_a_copy() {
        assert!(matches!(render_manifest(MANIFEST, &[]), Cow::Borrowed(_)));
        // A name nothing in the document carries changes nothing either.
        assert!(matches!(
            render_manifest(MANIFEST, &["not-a-component".to_owned()]),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_denied_component_leaves_the_profiles_alone_and_the_document_parsable() {
        let denied = vec!["rust-docs".to_owned()];
        let out = render_manifest(MANIFEST, &denied);
        let text = out.as_ref();

        // Its own target table says unavailable, in upstream's own spelling.
        assert!(text.contains("[pkg.rust-docs.target.x86_64-unknown-linux-gnu]\navailable = false"));
        assert!(
            !text.contains("rust-docs-1.98.1-x86_64-unknown-linux-gnu.tar.gz"),
            "the download URL went with the body"
        );
        // Every components/extensions entry naming it is gone.
        assert_eq!(text.matches("pkg = \"rust-docs\"").count(), 0);
        assert_eq!(
            text.matches("pkg = \"rustc\"").count(),
            1,
            "a component that is not denied keeps its entry"
        );
        // `[profiles]` is untouched: it tolerates a name the targets do not
        // carry, which is how rust-mingw sits in every profile already.
        assert!(text.contains("default = [\"rustc\", \"cargo\", \"rust-std\", \"rust-docs\"]"));
        assert!(text.contains("[renames.clippy]"));

        // The whole point of editing by section: it still parses.
        let parsed: toml::Value = toml::from_str(text).expect("filtered manifest still parses");
        let docs = &parsed["pkg"]["rust-docs"]["target"]["x86_64-unknown-linux-gnu"];
        assert_eq!(docs["available"].as_bool(), Some(false));
        assert!(
            docs.get("url").is_none(),
            "the URL is gone, not merely flagged"
        );
        let components = parsed["pkg"]["rust"]["target"]["x86_64-unknown-linux-gnu"]["components"]
            .as_array()
            .unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0]["pkg"].as_str(), Some("rustc"));
    }

    // ── the sidecar ───────────────────────────────────────────────────────────

    #[test]
    fn the_sidecar_is_the_coreutils_line_over_the_served_bytes() {
        let line = sidecar_line(b"", "channel-rust-stable.toml");
        assert_eq!(
            line,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  \
             channel-rust-stable.toml\n"
        );
        // rustup reads the first 64 characters as the hash and stores the first
        // 20 as the toolchain's update-hash.
        assert_eq!(line[..64].len(), 64);
        assert!(line[..64].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(&line[64..66], "  ");
    }

    // ── manifests.txt ─────────────────────────────────────────────────────────

    #[test]
    fn every_line_shape_the_release_tooling_writes_is_read() {
        let parsed = ManifestsTxt::parse(MANIFESTS_TXT);
        assert_eq!(parsed.rows.len(), 8);
        let coordinates: Vec<Option<&str>> = parsed
            .rows
            .iter()
            .map(|r| r.coordinate.as_deref())
            .collect();
        assert_eq!(
            coordinates,
            [
                Some("1.98.0"),
                Some("nightly-2026-09-01"),
                Some("1.98.1"),
                // A dated `stable` snapshot names no version.
                None,
                Some("nightly-2026-09-04"),
                Some("nightly-2026-09-05"),
                Some("beta-2026-09-11"),
                // The numbered beta spelling is the same release as the date's
                // `channel-rust-beta.toml`.
                Some("beta-2026-09-11"),
            ]
        );
        // Deduplicated, oldest first.
        assert_eq!(
            parsed.versions(),
            [
                "1.98.0",
                "nightly-2026-09-01",
                "1.98.1",
                "nightly-2026-09-04",
                "nightly-2026-09-05",
                "beta-2026-09-11"
            ]
        );
    }

    #[test]
    fn stable_repairs_to_the_newest_release_that_is_not_blocked() {
        let parsed = ManifestsTxt::parse(MANIFESTS_TXT);
        let stable = ToolchainName::parse("stable").unwrap();

        let none_blocked = |_: &str| false;
        assert_eq!(
            parsed
                .newest_allowed(&stable, &none_blocked, BACKTRACK_DAYS)
                .unwrap()
                .name,
            "1.98.1"
        );

        let newest_blocked = |v: &str| v == "1.98.1";
        let repaired = parsed
            .newest_allowed(&stable, &newest_blocked, BACKTRACK_DAYS)
            .unwrap();
        assert_eq!(repaired.name, "1.98.0");
        assert_eq!(repaired.date, "2026-08-20");

        // A downgrade is the block doing its job; with everything blocked there
        // is nothing to serve and the caller answers rustup's own not-found.
        assert!(parsed
            .newest_allowed(&stable, &|_| true, BACKTRACK_DAYS)
            .is_none());
    }

    #[test]
    fn a_partial_version_repairs_within_its_own_series() {
        let parsed = ManifestsTxt::parse(MANIFESTS_TXT);
        let series = ToolchainName::parse("1.98").unwrap();
        assert_eq!(
            parsed
                .newest_allowed(&series, &|v: &str| v == "1.98.1", BACKTRACK_DAYS)
                .unwrap()
                .name,
            "1.98.0"
        );
        let other = ToolchainName::parse("1.97").unwrap();
        assert!(
            parsed
                .newest_allowed(&other, &|_| false, BACKTRACK_DAYS)
                .is_none(),
            "a series the file does not carry repairs to nothing"
        );
    }

    #[test]
    fn a_dated_channel_walks_back_and_stops_at_the_horizon() {
        let parsed = ManifestsTxt::parse(MANIFESTS_TXT);
        let nightly = ToolchainName::parse("nightly").unwrap();

        // Newest first: 09-05, then 09-04, then 09-01.
        assert_eq!(
            parsed
                .newest_allowed(
                    &nightly,
                    &|v: &str| v == "nightly-2026-09-05",
                    BACKTRACK_DAYS
                )
                .unwrap()
                .date,
            "2026-09-04"
        );
        // With a horizon of two days, 09-01 is out of reach even though it is
        // allowed — the same limit rustup gives up at.
        assert!(parsed
            .newest_allowed(
                &nightly,
                &|v: &str| v.starts_with("nightly-2026-09-0") && v != "nightly-2026-09-01",
                2
            )
            .is_none());
        // And within the default horizon it is reachable.
        assert_eq!(
            parsed
                .newest_allowed(
                    &nightly,
                    &|v: &str| v == "nightly-2026-09-05" || v == "nightly-2026-09-04",
                    BACKTRACK_DAYS
                )
                .unwrap()
                .date,
            "2026-09-01"
        );
    }

    // ── dates ─────────────────────────────────────────────────────────────────

    #[test]
    fn a_release_day_is_its_earliest_instant() {
        let dt = parse_release_date("2026-09-03").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-09-03T00:00:00+00:00");
        assert!(parse_release_date("").is_none());
        assert!(parse_release_date("stable").is_none());
    }

    #[test]
    fn only_a_dated_version_carries_its_directory() {
        assert_eq!(date_of_version("nightly-2026-09-05"), Some("2026-09-05"));
        assert_eq!(date_of_version("beta-2026-09-11"), Some("2026-09-11"));
        assert_eq!(
            date_of_version("1.98.1"),
            None,
            "a stable release's date is in manifests.txt, not in its name"
        );
    }
}
