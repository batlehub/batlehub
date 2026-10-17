//! RubyGems: the versions API and the single-gem document.
//!
//! `/api/v1/versions/{name}.json` is an array of version objects keyed by
//! `number`. `/api/v1/gems/{name}.json` is a single object describing the gem
//! *at its newest version* — so it has no list to filter, and hiding a version
//! from it means rebuilding it around a different one.
//!
//! The Marshal indexes (`specs.4.8.gz`, `quick/Marshal.4.8/*`) are deliberately
//! not filtered; [`crate::entities::RegistryKind::listing_filter`] records why.

use serde_json::Value;

use super::{best_latest, BlockedVersions, MultiPackageBlocks};

/// Remove blocked versions from `/api/v1/versions/{name}.json`.
///
/// The array is newest-first as RubyGems serves it, and stays that way:
/// entries are filtered in place.
pub fn strip_versions(doc: &mut Value, blocked: &BlockedVersions) -> Vec<String> {
    let Some(items) = doc.as_array_mut() else {
        return Vec::new();
    };

    let mut removed = Vec::new();
    items.retain(|item| {
        let Some(v) = item.get("number").and_then(Value::as_str) else {
            return true;
        };
        if blocked.contains(v) {
            removed.push(v.to_owned());
            false
        } else {
            true
        }
    });
    removed
}

/// Repair `/api/v1/gems/{name}.json` — the document *is* one version, so when
/// that version is blocked it has to be rebuilt around another.
///
/// `available` is every version the *filtered* versions API still lists, which
/// is what the caller already has — so "is this version blocked" is answered by
/// its absence rather than by consulting the blocked set a second time, and the
/// two documents cannot disagree about what is visible.
///
/// The rebuilt document keeps the gem's package-level fields (name, homepage,
/// licences…) and moves `version` to the best survivor; the fields that describe
/// the *release* rather than the gem — checksum, download URL, the version's own
/// dates — are removed, because carrying the hidden release's checksum on a
/// different version would hand a client a hash that will never match what it
/// downloads.
///
/// Returns the version that was hidden, or `None` when nothing needed doing.
pub fn repair_gem(doc: &mut Value, available: &[String]) -> Option<String> {
    let current = doc.get("version").and_then(Value::as_str)?.to_owned();
    if available.contains(&current) {
        return None;
    }

    let obj = doc.as_object_mut()?;
    // Release-scoped fields. Left in place they would describe the hidden
    // release while `version` names a different one.
    for release_field in [
        "sha",
        "gem_uri",
        "spec_sha",
        "metadata",
        "built_at",
        "created_at",
        "version_created_at",
        "version_downloads",
        "requirements",
        "dependencies",
        "prerelease",
    ] {
        obj.remove(release_field);
    }

    match best_latest(available) {
        Some(best) => {
            obj.insert("version".to_owned(), Value::String(best));
        }
        None => {
            // Every version is blocked. Leaving `version` naming the blocked
            // release would advertise it; removing the key leaves a document
            // that describes a gem with no installable release, which is the
            // truth.
            obj.remove("version");
        }
    }
    Some(current)
}

// ── The compact index (RFC 0009 §7.3) ─────────────────────────────────────────
//
// What Bundler actually resolves from, and what this server did not serve — so
// every `bundle install` fell all the way back to `specs.4.8.gz`, the one index
// `listing_filter()` marks `Unsupported`. A blocked gem version was offered to
// the resolver, chosen, written to `Gemfile.lock`, and only then refused at
// download: exactly the mid-resolve failure RFC 0006 exists to prevent.
//
// Three plain-text documents, so `DocumentBody::Text` carries them and the
// "we would need a Marshal encoder" objection does not apply.

/// Filter `/info/{gem}` — one line per version.
///
/// ```text
/// ---
/// 1.0.0 |checksum:abc
/// 1.1.0 rack:>= 1.0|checksum:def,ruby:>= 2.5
/// ```
///
/// Line-oriented rather than a parse-and-re-render, for the same reason the
/// PyPI simple-page filter is: the format is one record per line, and anything
/// this function does not understand should survive untouched rather than be
/// dropped by a reserialisation that did not know about it.
pub fn strip_compact_info(text: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let mut removed = Vec::new();
    let mut out = String::with_capacity(text.len());
    // Each kept line verbatim, terminator included — see
    // `strip_compact_versions` for what `lines()` + `push('\n')` broke.
    for segment in text.split_inclusive('\n') {
        match compact_info_version(segment) {
            Some(v) if blocked.contains(v) => {
                removed.push(v.to_owned());
                continue;
            }
            _ => {}
        }
        out.push_str(segment);
    }
    *text = out;
    removed
}

/// The version a `/info` line declares, or `None` for the header and separator.
///
/// The version is the first whitespace-delimited field; a platform-specific
/// release spells it `1.0.0-java`, and the block is recorded against `1.0.0`,
/// so the platform suffix is trimmed before comparison.
fn compact_info_version(line: &str) -> Option<&str> {
    let line = line.trim_end();
    if line.is_empty() || line == "---" || line.starts_with("created_at:") {
        return None;
    }
    let field = line.split_whitespace().next()?;
    // `1.0.0-java` → `1.0.0`; a pre-release `1.0.0.pre` has no hyphen so is
    // unaffected, which is why this splits on `-` and not on `.`.
    Some(field.split('-').next().unwrap_or(field))
}

/// Filter `/versions` — the whole registry, one line per gem.
///
/// ```text
/// created_at: 2019-01-01T00:00:00Z
/// ---
/// rack 1.0.0,1.1.0 abc123…
/// rails 7.0.0 def456…
/// ```
///
/// Blocked versions are dropped from the comma-separated list, and a gem whose
/// every version is blocked loses its line entirely.
///
/// **The checksum is rewritten when — and only when — the line changed.** It is
/// the md5 of the gem's `/info` document, and Bundler uses it to decide whether
/// to re-fetch that document. Leaving it alone after filtering would let a
/// client keep serving an `/info` copy it fetched before the block, so the
/// block would not reach the resolver until something else invalidated it.
/// Rewriting it only on change keeps the common no-blocks case byte-identical
/// to upstream, so no client re-downloads anything it did not need to.
pub fn strip_compact_versions(text: &mut String, blocked: &MultiPackageBlocks) -> Vec<String> {
    let mut removed = Vec::new();
    let mut out = String::with_capacity(text.len());

    // `split_inclusive` rather than `lines()`: each line keeps its own
    // terminator, so a CRLF or unterminated last line comes back as it came
    // in. `lines()` dropped the `\r` of a CRLF and added a `\n` to an
    // unterminated line, so the "untouched" path was not byte-identical and a
    // second pass rewrote the first pass's output (found by
    // `fuzz_listing_filter`).
    for segment in text.split_inclusive('\n') {
        let line = segment
            .strip_suffix('\n')
            .map_or(segment, |l| l.strip_suffix('\r').unwrap_or(l));
        let ending = &segment[line.len()..];
        let Some((name, versions, checksum)) = split_compact_versions_line(line) else {
            out.push_str(segment);
            continue;
        };

        let kept: Vec<&str> = versions
            .split(',')
            .filter(|v| {
                let bare = v.split('-').next().unwrap_or(v);
                if blocked.contains(name, bare) {
                    removed.push(format!("{name}-{bare}"));
                    false
                } else {
                    true
                }
            })
            .collect();

        if kept.len() == versions.split(',').count() {
            // Untouched: emit the upstream bytes exactly.
            out.push_str(segment);
            continue;
        }
        if kept.is_empty() {
            // Every version blocked: the gem is not offered at all.
            continue;
        }

        out.push_str(name);
        out.push(' ');
        out.push_str(&kept.join(","));
        out.push(' ');
        out.push_str(&rewritten_info_checksum(checksum, &kept));
        out.push_str(ending);
    }

    *text = out;
    removed
}

/// `name versions checksum` — the three fields of a `/versions` line.
///
/// `None` for the `created_at:` header, the `---` separator, and anything else
/// that does not have exactly the shape we know how to edit. Such a line is
/// copied through rather than dropped.
fn split_compact_versions_line(line: &str) -> Option<(&str, &str, &str)> {
    let line = line.trim_end();
    if line.is_empty() || line == "---" || line.starts_with("created_at:") {
        return None;
    }
    let mut parts = line.split(' ');
    let name = parts.next()?;
    let versions = parts.next()?;
    let checksum = parts.next()?;
    if parts.next().is_some() || name.is_empty() || versions.is_empty() {
        return None;
    }
    Some((name, versions, checksum))
}

/// A checksum that changes when the filtered version list changes.
///
/// Bundler treats this field as opaque — it compares it against the value it
/// stored last time to decide whether to re-fetch `/info/{gem}` — so it only
/// has to be *stable* and *different*, not a real md5 of anything. Derived from
/// the upstream checksum plus the surviving versions, and rendered 32 hex
/// characters wide so it still looks like the md5 the format implies.
fn rewritten_info_checksum(upstream: &str, kept: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(upstream.as_bytes());
    h.update(b"\0");
    h.update(kept.join(",").as_bytes());
    let full = h.finalize();
    full.iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;
    use serde_json::json;

    fn blocked(vs: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Rubygems,
            vs.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    fn versions_doc() -> Value {
        json!([
            { "number": "7.1.0", "sha": "aaa" },
            { "number": "7.0.0", "sha": "bbb" },
            { "number": "6.1.0", "sha": "ccc" }
        ])
    }

    #[test]
    fn versions_api_drops_the_blocked_entry_and_keeps_order() {
        let mut doc = versions_doc();
        let removed = strip_versions(&mut doc, &blocked(&["7.0.0"]));

        assert_eq!(removed, vec!["7.0.0".to_owned()]);
        let numbers: Vec<&str> = doc
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["number"].as_str().unwrap())
            .collect();
        assert_eq!(numbers, ["7.1.0", "6.1.0"], "still newest-first");
    }

    #[test]
    fn versions_api_blocking_everything_leaves_an_empty_array() {
        let mut doc = versions_doc();
        strip_versions(&mut doc, &blocked(&["7.1.0", "7.0.0", "6.1.0"]));

        assert_eq!(doc, json!([]));
    }

    #[test]
    fn versions_api_blocking_an_absent_version_changes_nothing() {
        let mut doc = versions_doc();
        let before = doc.clone();
        assert!(strip_versions(&mut doc, &blocked(&["9.9.9"])).is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn a_malformed_versions_document_is_returned_unchanged() {
        let mut doc = json!({ "not": "an array" });
        let before = doc.clone();
        assert!(strip_versions(&mut doc, &blocked(&["1.0.0"])).is_empty());
        assert_eq!(doc, before);
    }

    fn gem_doc() -> Value {
        json!({
            "name": "rails",
            "version": "7.1.0",
            "sha": "deadbeef",
            "gem_uri": "https://rubygems.org/gems/rails-7.1.0.gem",
            "homepage_uri": "https://rubyonrails.org",
            "licenses": ["MIT"]
        })
    }

    #[test]
    fn gem_document_moves_to_the_best_surviving_version() {
        let mut doc = gem_doc();
        let removed = repair_gem(&mut doc, &["7.0.0".to_owned(), "6.1.0".to_owned()]);

        assert_eq!(removed, Some("7.1.0".to_owned()));
        assert_eq!(doc["version"], json!("7.0.0"));
        assert_eq!(
            doc["homepage_uri"],
            json!("https://rubyonrails.org"),
            "gem-level fields survive"
        );
    }

    /// The checksum and download URL belonged to the hidden release. Carried
    /// onto a different version they are a hash that can never match.
    #[test]
    fn gem_document_drops_the_hidden_release_s_own_fields() {
        let mut doc = gem_doc();
        repair_gem(&mut doc, &["7.0.0".to_owned()]);

        assert!(doc.get("sha").is_none());
        assert!(doc.get("gem_uri").is_none());
    }

    #[test]
    fn gem_document_naming_a_still_listed_version_is_untouched() {
        let mut doc = gem_doc();
        let before = doc.clone();
        assert!(repair_gem(&mut doc, &["7.1.0".to_owned(), "6.1.0".to_owned()]).is_none());
        assert_eq!(doc, before);
    }

    #[test]
    fn gem_document_with_every_version_blocked_names_none() {
        let mut doc = gem_doc();
        repair_gem(&mut doc, &[]);

        assert!(doc.get("version").is_none());
        assert_eq!(doc["name"], json!("rails"));
    }
}

#[cfg(test)]
mod compact_index_tests {
    use super::*;

    fn blocked_versions(vs: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            crate::entities::RegistryKind::Rubygems,
            vs.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    fn blocked_pairs(pairs: &[(&str, &str)]) -> MultiPackageBlocks {
        MultiPackageBlocks::new(
            crate::entities::RegistryKind::Rubygems,
            pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    const INFO: &str = "---\n\
        1.0.0 |checksum:aaa\n\
        1.1.0 rack:>= 1.0|checksum:bbb,ruby:>= 2.5\n\
        2.0.0 |checksum:ccc\n";

    #[test]
    fn info_drops_only_the_blocked_version() {
        let mut text = INFO.to_owned();
        let removed = strip_compact_info(&mut text, &blocked_versions(&["1.1.0"]));
        assert_eq!(removed, vec!["1.1.0"]);
        assert!(text.contains("1.0.0 |checksum:aaa"));
        assert!(text.contains("2.0.0 |checksum:ccc"));
        assert!(!text.contains("1.1.0"));
    }

    /// The `---` separator is what tells Bundler the header is over. Dropping
    /// it would make the document unparseable rather than shorter.
    #[test]
    fn info_keeps_the_separator_and_the_untouched_lines_verbatim() {
        let mut text = INFO.to_owned();
        strip_compact_info(&mut text, &blocked_versions(&["1.1.0"]));
        assert!(text.starts_with("---\n"));
        assert!(!text.contains("rack:>= 1.0"));
        assert!(text.contains("1.0.0 |checksum:aaa\n"));
    }

    #[test]
    fn info_with_nothing_blocked_is_unchanged_except_for_line_endings() {
        let mut text = INFO.to_owned();
        let removed = strip_compact_info(&mut text, &blocked_versions(&[]));
        assert!(removed.is_empty());
        assert_eq!(text, INFO);
    }

    /// A platform release spells the version `1.0.0-java`; the block is
    /// recorded against `1.0.0` and must reach both.
    #[test]
    fn info_matches_a_platform_specific_release() {
        let mut text = "---\n1.0.0 |checksum:a\n1.0.0-java |checksum:b\n".to_owned();
        let removed = strip_compact_info(&mut text, &blocked_versions(&["1.0.0"]));
        assert_eq!(removed.len(), 2);
        assert_eq!(text, "---\n");
    }

    const VERSIONS: &str = "created_at: 2019-01-01T00:00:00Z\n\
        ---\n\
        rack 1.0.0,1.1.0 abc123\n\
        rails 7.0.0 def456\n";

    #[test]
    fn versions_drops_the_blocked_entry_from_the_list() {
        let mut text = VERSIONS.to_owned();
        let removed = strip_compact_versions(&mut text, &blocked_pairs(&[("rack", "1.1.0")]));
        assert_eq!(removed, vec!["rack-1.1.0"]);
        assert!(text.contains("rack 1.0.0 "));
        assert!(!text.contains("1.1.0"));
        assert!(text.contains("rails 7.0.0 def456"));
    }

    /// A gem whose every version is blocked is not offered at all — leaving an
    /// empty version list would be a line Bundler cannot parse.
    #[test]
    fn versions_drops_the_line_when_every_version_is_blocked() {
        let mut text = VERSIONS.to_owned();
        strip_compact_versions(
            &mut text,
            &blocked_pairs(&[("rack", "1.0.0"), ("rack", "1.1.0")]),
        );
        assert!(!text.contains("rack"));
        assert!(text.contains("rails 7.0.0 def456"));
    }

    #[test]
    fn versions_keeps_the_header_and_separator() {
        let mut text = VERSIONS.to_owned();
        strip_compact_versions(&mut text, &blocked_pairs(&[("rack", "1.1.0")]));
        assert!(text.starts_with("created_at: 2019-01-01T00:00:00Z\n---\n"));
    }

    /// The checksum keys Bundler's cached copy of `/info/{gem}`. If it did not
    /// move when the version list did, a client could keep serving an `/info`
    /// it fetched before the block — so the block would never reach the
    /// resolver.
    #[test]
    fn a_filtered_line_gets_a_new_info_checksum() {
        let mut text = VERSIONS.to_owned();
        strip_compact_versions(&mut text, &blocked_pairs(&[("rack", "1.1.0")]));
        let rack = text.lines().find(|l| l.starts_with("rack ")).unwrap();
        let checksum = rack.split(' ').nth(2).unwrap();
        assert_ne!(checksum, "abc123");
        assert_eq!(checksum.len(), 32, "still md5-shaped");
    }

    /// ...but an untouched line must keep upstream's bytes exactly, or every
    /// client re-downloads every `/info` document on every block change.
    #[test]
    fn an_untouched_line_keeps_the_upstream_checksum() {
        let mut text = VERSIONS.to_owned();
        strip_compact_versions(&mut text, &blocked_pairs(&[("rack", "1.1.0")]));
        assert!(text.contains("rails 7.0.0 def456"));
    }

    #[test]
    fn versions_with_nothing_blocked_is_byte_identical() {
        let mut text = VERSIONS.to_owned();
        let removed = strip_compact_versions(&mut text, &blocked_pairs(&[]));
        assert!(removed.is_empty());
        assert_eq!(text, VERSIONS);
    }

    /// Fail open: a line this filter does not understand is copied through
    /// rather than dropped, matching the module's stated failure mode.
    #[test]
    fn an_unparseable_line_survives() {
        let mut text = "---\nthis line has too many fields to be a versions line\n".to_owned();
        let before = text.clone();
        strip_compact_versions(&mut text, &blocked_pairs(&[("rack", "1.0.0")]));
        assert_eq!(text, before);
    }

    /// Line endings survive a filter, so a second pass over the output is a
    /// no-op: a CRLF line keeps its `\r`, an unterminated last line gains no
    /// `\n`, and a document with no hit is byte-identical.
    #[test]
    fn compact_versions_keeps_each_line_s_own_ending() {
        let blocks = blocked_pairs(&[("rack", "1.0.0")]);
        let mut text = "---\r\nrack 1.0.0,1.1.0 abc\r\nrails 7.0.0 def".to_owned();
        strip_compact_versions(&mut text, &blocks);
        assert!(text.starts_with("---\r\nrack 1.1.0 "), "{text:?}");
        assert!(text.ends_with("\r\nrails 7.0.0 def"), "{text:?}");
        let once = text.clone();
        assert!(strip_compact_versions(&mut text, &blocks).is_empty());
        assert_eq!(text, once);

        let mut untouched = "rails 7.0.0 def\r".to_owned();
        strip_compact_versions(&mut untouched, &blocks);
        assert_eq!(untouched, "rails 7.0.0 def\r");

        // `/info/{gem}` too.
        let mut info = "---\r\n1.0.0 |checksum:a\r\n1.1.0 |checksum:b".to_owned();
        strip_compact_info(&mut info, &blocked_versions(&["1.0.0"]));
        assert_eq!(info, "---\r\n1.1.0 |checksum:b");
        let mut untouched = "---\r\n1.1.0 |checksum:b\r".to_owned();
        strip_compact_info(&mut untouched, &blocked_versions(&["1.0.0"]));
        assert_eq!(untouched, "---\r\n1.1.0 |checksum:b\r");
    }
}
