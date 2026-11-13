//! mise's versions host: `data/<tool>.toml`, read once for every reader.
//!
//! RFC 0043 §5.1. The document is one `[versions]` table, one entry per line,
//! in the order mise reads it (an `IndexMap` in `parse_version_list`):
//!
//! ```toml
//! [versions]
//! "1.5" = { created_at = 2015-08-16T06:39:17.000Z, release_url = "…" }
//! ```
//!
//! Each entry line is a complete TOML document on its own, so it is parsed
//! with `toml` one line at a time rather than by hand — and the readers never
//! re-serialise, they keep or drop whole lines, so formatting, order and keys
//! this proxy does not know survive byte-exact. A line that does not parse is
//! reported as unreadable and its caller keeps it: a version left in is visible,
//! a corrupted document is not.

use chrono::{DateTime, Utc};

/// One line of the `[versions]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// An entry: the version as mise spells it, and its `created_at`.
    Entry {
        version: String,
        created_at: Option<DateTime<Utc>>,
    },
    /// Inside `[versions]`, not blank, and not an entry this parser can read.
    Unreadable,
    /// A header, a blank line, a comment, or anything outside `[versions]`.
    Other,
}

/// Classify every line of `body`, in `str::lines` order.
pub fn lines(body: &str) -> Vec<Line> {
    let mut in_versions = false;
    body.lines()
        .map(|raw| {
            let line = raw.trim();
            if line.starts_with('[') {
                in_versions = line == "[versions]";
                return Line::Other;
            }
            if !in_versions || line.is_empty() || line.starts_with('#') {
                return Line::Other;
            }
            parse_entry(line).unwrap_or(Line::Unreadable)
        })
        .collect()
}

fn parse_entry(line: &str) -> Option<Line> {
    let table: toml::Table = toml::from_str(line).ok()?;
    let mut entries = table.into_iter();
    let (version, value) = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    let created_at = value
        .get("created_at")
        .and_then(toml::Value::as_datetime)
        .and_then(|d| DateTime::parse_from_rfc3339(&d.to_string()).ok())
        .map(|d| d.with_timezone(&Utc));
    Some(Line::Entry {
        version,
        created_at,
    })
}

/// Every version and its `created_at`, in document order — for the console's
/// version table.
pub fn versions(body: &str) -> Vec<(String, Option<DateTime<Utc>>)> {
    lines(body)
        .into_iter()
        .filter_map(|l| match l {
            Line::Entry {
                version,
                created_at,
            } => Some((version, created_at)),
            _ => None,
        })
        .collect()
}

/// The versions published less than `min_age` before `now`: what a
/// `release_age_gate` on this registry hides from the listing (RFC 0043 §11,
/// decision 2). An entry without a readable `created_at` is hidden only under
/// `deny_missing_timestamp`, as the gate treats an artifact with no date
/// (review of #311).
///
/// shortcut: a line the parser cannot read at all has no version to name, so
/// it stays listed even under `deny_missing_timestamp`; drop such lines in
/// `strip_versions_toml` if the upstream format ever produces them.
pub fn younger_than(
    body: &str,
    gate: crate::rules::ListingAgeGate,
    now: DateTime<Utc>,
) -> Vec<String> {
    let Ok(min_age) = chrono::Duration::from_std(gate.min_age) else {
        return Vec::new();
    };
    versions(body)
        .into_iter()
        .filter(|(_, at)| match at {
            Some(at) => now - *at < min_age,
            None => gate.deny_missing_timestamp,
        })
        .map(|(v, _)| v)
        .collect()
}

/// One line of a `python-precompiled-<triple>.gz` index, decompressed: the
/// version mise reads from it and the build's release date.
///
/// The index is one python-build-standalone file name per line. mise 2026.9.5
/// (`src/plugins/core/python.rs`, `fetch_precompiled_remote_versions` and
/// `select_python_precompiled`) reads a line only when it matches
/// `^cpython-(\d+\.\d+\.[\da-z]+)\+(\d+)`: the version, then the release tag,
/// a `YYYYMMDD` date. `None` for any other line, which mise ignores too. The
/// date is `None` when the tag is not a calendar date.
pub fn precompiled_entry(line: &str) -> Option<(&str, Option<DateTime<Utc>>)> {
    let (version, tail) = line.strip_prefix("cpython-")?.split_once('+')?;
    let numeric = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let mut parts = version.splitn(3, '.');
    let well_formed = parts.next().is_some_and(numeric)
        && parts.next().is_some_and(numeric)
        && parts.next().is_some_and(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        });
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if !well_formed || digits == 0 {
        return None;
    }
    let date = tail
        .get(..8)
        .filter(|_| digits >= 8)
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y%m%d").ok())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc());
    Some((version, date))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_precompiled_line_is_read_as_mise_reads_it() {
        let (v, at) = precompiled_entry(
            "cpython-3.16.0a2+20261110-x86_64-unknown-linux-gnu-install_only.tar.gz",
        )
        .unwrap();
        assert_eq!(v, "3.16.0a2");
        assert_eq!(at.unwrap().to_rfc3339(), "2026-11-10T00:00:00+00:00");
        // The 2020 spelling has no `+<tag>`: mise's regex skips it, so do we.
        assert_eq!(
            precompiled_entry("cpython-3.7.7-x86_64-unknown-linux-gnu-pgo-20200418T2226.tar.zst"),
            None
        );
        assert_eq!(precompiled_entry("cpython-3.12+20240101-x.tar.gz"), None);
        assert_eq!(precompiled_entry("pypy-3.12.1+20240101-x.tar.gz"), None);
        assert_eq!(
            precompiled_entry("cpython-3.12.1+2024-x.tar.gz"),
            Some(("3.12.1", None)),
            "a tag that is not a date is still an entry, undated"
        );
    }

    /// The head of the real `data/jq.toml`, probed 2026-11-10.
    pub(crate) const JQ: &str = r#"[versions]
"1.3" = { created_at = 2015-08-08T05:03:24.000Z, release_url = "https://github.com/jqlang/jq/releases/tag/jq-1.3" }
"1.5rc1" = { created_at = 2015-01-01T02:54:01.000Z, release_url = "https://github.com/jqlang/jq/releases/tag/jq-1.5rc1", prerelease = true }
"1.8.1" = { created_at = 2025-07-01T11:50:06.000Z, release_url = "https://github.com/jqlang/jq/releases/tag/jq-1.8.1" }
"1.8.2" = { created_at = 2026-06-20T14:11:27.000Z, release_url = "https://github.com/jqlang/jq/releases/tag/jq-1.8.2" }
"#;

    #[test]
    fn reads_every_entry_in_document_order() {
        let v: Vec<String> = versions(JQ).into_iter().map(|(v, _)| v).collect();
        assert_eq!(v, ["1.3", "1.5rc1", "1.8.1", "1.8.2"]);
        assert_eq!(
            versions(JQ)[3].1.unwrap().to_rfc3339(),
            "2026-06-20T14:11:27+00:00"
        );
    }

    #[test]
    fn a_line_outside_the_table_or_unparseable_is_told_apart() {
        let body = "# head\n[versions]\n\"1.0\" = { created_at = 2020-01-01T00:00:00Z }\nnot toml at all\n[other]\n\"2.0\" = { created_at = 2020-01-01T00:00:00Z }\n";
        let l = lines(body);
        assert_eq!(l[0], Line::Other);
        assert!(matches!(&l[2], Line::Entry { version, .. } if version == "1.0"));
        assert_eq!(l[3], Line::Unreadable);
        assert_eq!(l[5], Line::Other, "an entry outside [versions] is not one");
    }

    #[test]
    fn the_age_gate_names_only_versions_younger_than_it() {
        let now = DateTime::parse_from_rfc3339("2026-11-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let day = std::time::Duration::from_secs(86_400);
        let gate = |min_age| crate::rules::ListingAgeGate {
            min_age,
            deny_missing_timestamp: false,
        };
        assert_eq!(younger_than(JQ, gate(day * 200), now), ["1.8.2"]);
        assert!(younger_than(JQ, gate(day), now).is_empty());
        assert_eq!(younger_than(JQ, gate(day * 365 * 30), now).len(), 4);
    }

    /// A date the parser cannot read (a local date here) fails open by
    /// default and closed under `deny_missing_timestamp` (RFC 0043 §6.2).
    #[test]
    fn deny_missing_timestamp_hides_an_undated_entry() {
        let body = "[versions]\n\"1.8.1\" = { created_at = 2025-07-01T11:50:06.000Z }\n\"1.9.0\" = { created_at = 2026-11-09 }\n";
        assert_eq!(versions(body)[1], ("1.9.0".to_owned(), None));
        let now = Utc::now();
        let min_age = std::time::Duration::from_secs(86_400);
        let gate = |deny_missing_timestamp| crate::rules::ListingAgeGate {
            min_age,
            deny_missing_timestamp,
        };
        assert!(younger_than(body, gate(false), now).is_empty());
        assert_eq!(younger_than(body, gate(true), now), ["1.9.0"]);
    }
}
