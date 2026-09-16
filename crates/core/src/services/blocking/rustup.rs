//! The Rust toolchain tree: `manifests.txt`.
//!
//! RFC 0024 §6.2. This file holds the one rustup document that is a *list* of
//! releases. The channel manifests are not: each describes a single release,
//! and what happens to a blocked one — a `404` for an exact name, a repair to
//! the newest allowed release for an alias — is decided in the handler, which
//! is the only layer that can fetch the second document a repair needs.
//!
//! `manifests.txt` is read by people and by scripts, never by rustup. It is
//! filtered anyway, for RFC 0010 §4.4's reason: leaving a blocked release in a
//! second document that answers the same question as the first is leaving an
//! unfiltered answer lying around.

use super::BlockedVersions;
use crate::services::rustup::ManifestsTxt;

/// Remove every line of `manifests.txt` that names a blocked release.
///
/// Line order and every surviving line are untouched: the file is a list of
/// paths, and a rewrite of anything but whole lines would change a document
/// mirrors diff against upstream's.
///
/// A line whose coordinate cannot be read from the path alone — a dated
/// `channel-rust-stable.toml` snapshot names no version — is kept.
/// Over-listing is the safe direction, and the release it points at is still
/// refused at the manifest and at the download gate.
pub fn strip_manifests_txt(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let parsed = ManifestsTxt::parse(body);
    if parsed.rows.is_empty() {
        tracing::warn!("manifests.txt names no manifest paths; passing through unfiltered");
        return Vec::new();
    }

    let mut removed: Vec<String> = Vec::new();
    let trailing_newline = body.ends_with('\n');
    let kept: Vec<&str> = body
        .lines()
        .filter(|line| {
            let Some(row) = crate::services::rustup::ManifestsTxt::parse(line)
                .rows
                .pop()
            else {
                return true;
            };
            match row.coordinate.as_deref() {
                Some(c) if blocked.contains(c) => {
                    removed.push(c.to_owned());
                    false
                }
                _ => true,
            }
        })
        .collect();

    if removed.is_empty() {
        return removed;
    }
    let mut out = kept.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    *body = out;
    removed.sort();
    removed.dedup();
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    const MANIFESTS: &str = "static.rust-lang.org/dist/2026-09-03/channel-rust-1.98.1.toml\n\
        static.rust-lang.org/dist/2026-09-05/channel-rust-nightly.toml\n\
        static.rust-lang.org/dist/2026-09-05/channel-rust-stable.toml\n\
        static.rust-lang.org/dist/2026-09-11/channel-rust-beta.toml\n\
        static.rust-lang.org/dist/2026-09-11/channel-rust-1.99.0-beta.5.toml\n";

    fn blocked(versions: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Rustup,
            versions.iter().map(|v| (*v).to_owned()).collect(),
        )
    }

    #[test]
    fn a_blocked_stable_release_loses_its_line_and_nothing_else() {
        let mut body = MANIFESTS.to_owned();
        let removed = strip_manifests_txt(&mut body, &blocked(&["1.98.1"]));
        assert_eq!(removed, ["1.98.1"]);
        assert!(!body.contains("channel-rust-1.98.1.toml"));
        assert_eq!(body.lines().count(), 4);
        assert!(body.ends_with('\n'), "the trailing newline survives");
    }

    #[test]
    fn a_blocked_nightly_is_matched_by_its_dated_coordinate() {
        let mut body = MANIFESTS.to_owned();
        let removed = strip_manifests_txt(&mut body, &blocked(&["nightly-2026-09-05"]));
        assert_eq!(removed, ["nightly-2026-09-05"]);
        assert!(!body.contains("2026-09-05/channel-rust-nightly.toml"));
        assert!(
            body.contains("2026-09-05/channel-rust-stable.toml"),
            "a dated stable snapshot names no version and is kept"
        );
    }

    #[test]
    fn both_beta_spellings_of_one_date_go_together() {
        let mut body = MANIFESTS.to_owned();
        let removed = strip_manifests_txt(&mut body, &blocked(&["beta-2026-09-11"]));
        assert_eq!(removed, ["beta-2026-09-11"]);
        assert!(!body.contains("channel-rust-beta.toml"));
        assert!(
            !body.contains("channel-rust-1.99.0-beta.5.toml"),
            "the numbered spelling is the same release on the same date"
        );
    }

    #[test]
    fn nothing_blocked_changes_nothing() {
        let mut body = MANIFESTS.to_owned();
        assert!(strip_manifests_txt(&mut body, &blocked(&["1.0.0"])).is_empty());
        assert_eq!(body, MANIFESTS);
    }

    #[test]
    fn a_document_that_is_not_the_listing_passes_through() {
        let mut body = "not a manifest list\n".to_owned();
        assert!(strip_manifests_txt(&mut body, &blocked(&["1.98.1"])).is_empty());
        assert_eq!(body, "not a manifest list\n");
    }
}
