//! mise's versions host: `data/<tool>.toml` (RFC 0043 §6.2).
//!
//! The listing mise resolves `latest` through before any backend, so a removed
//! entry is a version `mise use jq@latest` never picks. Whole lines are dropped
//! and nothing is re-serialised: order (which mise reads), formatting and keys
//! this proxy does not know survive, and a line the parser cannot read is kept.

use super::BlockedVersions;
use crate::services::mise_versions::{lines, precompiled_entry, Line};

/// Remove blocked entries from a `[versions]` table. Returns what was removed.
pub fn strip_versions_toml(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let classified = lines(body);
    let dropped: Vec<String> = classified
        .iter()
        .filter_map(|l| match l {
            Line::Entry { version, .. } if blocked.contains(version) => Some(version.clone()),
            _ => None,
        })
        .collect();
    if dropped.is_empty() {
        return dropped;
    }
    let trailing_newline = body.ends_with('\n');
    let kept: Vec<&str> = body
        .lines()
        .zip(&classified)
        .filter(|(_, l)| !matches!(l, Line::Entry { version, .. } if blocked.contains(version)))
        .map(|(raw, _)| raw)
        .collect();
    let mut out = kept.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    *body = out;
    dropped
}

/// Remove from a decompressed `python-precompiled-<triple>.gz` index every
/// build of a blocked Python version and, under `gate`, every build released
/// less than `min_age` ago (RFC 0043 decision 3). The gate judges the build,
/// not the version: an older build of the same version stays, as the gate
/// would let its archive through. A line mise does not read is kept. Returns
/// how many lines were removed; `0` leaves `body` untouched.
pub fn strip_precompiled_index(
    body: &mut String,
    blocked: &BlockedVersions,
    gate: Option<crate::rules::ListingAgeGate>,
    now: chrono::DateTime<chrono::Utc>,
) -> usize {
    let min_age = gate.and_then(|g| chrono::Duration::from_std(g.min_age).ok());
    let hidden = |line: &str| {
        let Some((version, released)) = precompiled_entry(line) else {
            return false;
        };
        blocked.contains(version)
            || match (gate, min_age, released) {
                (Some(_), Some(min_age), Some(at)) => now - at < min_age,
                (Some(g), _, None) => g.deny_missing_timestamp,
                _ => false,
            }
    };
    let total = body.lines().count();
    let kept: Vec<&str> = body.lines().filter(|l| !hidden(l)).collect();
    let removed = total - kept.len();
    if removed > 0 {
        let trailing_newline = body.ends_with('\n');
        let mut out = kept.join("\n");
        if trailing_newline && !out.is_empty() {
            out.push('\n');
        }
        *body = out;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    const DOC: &str = "[versions]\n\"1.8.1\" = { created_at = 2025-07-01T11:50:06.000Z, x_unknown = 1 }\n\"1.8.2\" = { created_at = 2026-06-20T14:11:27.000Z, rolling = false }\n\"nightly\" = { created_at = 2026-06-21T00:00:00Z, rolling = true }\n";

    fn blocked(v: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::MiseVersions,
            v.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    #[test]
    fn nothing_blocked_is_byte_identical() {
        let mut body = DOC.to_owned();
        assert!(strip_versions_toml(&mut body, &blocked(&[])).is_empty());
        assert_eq!(body, DOC);
    }

    #[test]
    fn a_blocked_entry_is_one_line_less_and_still_parses() {
        let mut body = DOC.to_owned();
        assert_eq!(
            strip_versions_toml(&mut body, &blocked(&["1.8.2"])),
            ["1.8.2"]
        );
        assert_eq!(
            body,
            "[versions]\n\"1.8.1\" = { created_at = 2025-07-01T11:50:06.000Z, x_unknown = 1 }\n\"nightly\" = { created_at = 2026-06-21T00:00:00Z, rolling = true }\n"
        );
        let t: toml::Table = toml::from_str(&body).unwrap();
        assert_eq!(t["versions"].as_table().unwrap().len(), 2);
    }

    #[test]
    fn every_entry_blocked_leaves_an_empty_table() {
        let mut body = DOC.to_owned();
        strip_versions_toml(&mut body, &blocked(&["1.8.1", "1.8.2", "nightly"]));
        assert_eq!(body, "[versions]\n");
        let t: toml::Table = toml::from_str(&body).unwrap();
        assert!(t["versions"].as_table().unwrap().is_empty());
    }

    /// Lines in the shape of the real index (jdx.dev, probed 2026-11-13).
    const INDEX: &str = "cpython-3.7.7-x86_64-unknown-linux-gnu-pgo-20200418T2226.tar.zst\n\
cpython-3.12.1+20240107-x86_64-unknown-linux-gnu-install_only.tar.gz\n\
cpython-3.12.1+20261110-x86_64-unknown-linux-gnu-install_only.tar.gz\n\
cpython-3.13.0+20261110-x86_64-unknown-linux-gnu-install_only.tar.gz\n";

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    #[test]
    fn precompiled_nothing_hidden_is_untouched() {
        let mut body = INDEX.to_owned();
        let n = strip_precompiled_index(&mut body, &blocked(&[]), None, at("2026-11-13T00:00:00Z"));
        assert_eq!((n, body.as_str()), (0, INDEX));
    }

    #[test]
    fn precompiled_a_blocked_version_loses_every_build() {
        let mut body = INDEX.to_owned();
        let n = strip_precompiled_index(
            &mut body,
            &blocked(&["3.12.1"]),
            None,
            at("2026-11-13T00:00:00Z"),
        );
        assert_eq!(n, 2);
        assert!(!body.contains("3.12.1"), "{body}");
        assert!(
            body.starts_with("cpython-3.7.7-"),
            "a line mise skips stays"
        );
        assert!(body.ends_with("3.13.0+20261110-x86_64-unknown-linux-gnu-install_only.tar.gz\n"));
    }

    /// The gate judges the build: 3.12.1's 2024 build stays, its 2026 one goes.
    #[test]
    fn precompiled_the_age_gate_hides_young_builds_only() {
        let mut body = INDEX.to_owned();
        let gate = crate::rules::ListingAgeGate {
            min_age: std::time::Duration::from_secs(7 * 86_400),
            deny_missing_timestamp: false,
        };
        let n = strip_precompiled_index(
            &mut body,
            &blocked(&[]),
            Some(gate),
            at("2026-11-13T00:00:00Z"),
        );
        assert_eq!(n, 2);
        assert!(body.contains("3.12.1+20240107"), "{body}");
        assert!(!body.contains("20261110"), "{body}");
    }

    #[test]
    fn precompiled_an_undated_build_follows_deny_missing_timestamp() {
        let undated = "cpython-3.12.1+1-x86_64-unknown-linux-gnu-install_only.tar.gz\n";
        for (deny, removed) in [(false, 0), (true, 1)] {
            let mut body = undated.to_owned();
            let gate = crate::rules::ListingAgeGate {
                min_age: std::time::Duration::from_secs(86_400),
                deny_missing_timestamp: deny,
            };
            let n =
                strip_precompiled_index(&mut body, &blocked(&[]), Some(gate), chrono::Utc::now());
            assert_eq!(n, removed, "deny_missing_timestamp = {deny}");
        }
    }

    #[test]
    fn an_unreadable_line_is_kept() {
        let mut body = format!("{DOC}garbage line\n");
        strip_versions_toml(&mut body, &blocked(&["1.8.1"]));
        assert!(body.ends_with("garbage line\n"));
    }
}
