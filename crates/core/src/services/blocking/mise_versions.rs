//! mise's versions host: `data/<tool>.toml` (RFC 0043 §6.2).
//!
//! The listing mise resolves `latest` through before any backend, so a removed
//! entry is a version `mise use jq@latest` never picks. Whole lines are dropped
//! and nothing is re-serialised: order (which mise reads), formatting and keys
//! this proxy does not know survive, and a line the parser cannot read is kept.

use super::BlockedVersions;
use crate::services::mise_versions::{lines, Line};

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

    #[test]
    fn an_unreadable_line_is_kept() {
        let mut body = format!("{DOC}garbage line\n");
        strip_versions_toml(&mut body, &blocked(&["1.8.1"]));
        assert!(body.ends_with("garbage line\n"));
    }
}
