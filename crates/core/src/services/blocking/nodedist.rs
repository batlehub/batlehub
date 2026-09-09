//! The `nodejs.org/dist` tree: `index.tab` and `index.json`.
//!
//! RFC 0010 §6.2. `index.tab` is the enforcement chokepoint for nvm:
//! `nvm_remote_version` resolves *every* install through it, including a fully
//! specified `nvm install 22.11.0`, and sets `VERSION='N/A'` when the pattern
//! matches nothing. A blocked row therefore produces nvm's own *"Version
//! '22.11.0' not found"* and no download. `index.json` is the same table for
//! fnm and mise.
//!
//! There is no "newest pointer" to repair in either document: `index.tab` is
//! ordered newest-first and nvm derives its `lts/*` aliases from the `lts`
//! column of whatever rows survive, so removing a row moves the alias by
//! construction.
//!
//! **Line 1 is kept, always.** nvm strips it with `sed 1d` unconditionally, so
//! a lost header would silently eat the newest release rather than the header.

use serde_json::Value;

use super::BlockedVersions;
use crate::services::nodedist::IndexTab;

/// Remove blocked rows from an `index.tab` body, header preserved.
///
/// Rows are matched on their `version` column by the header's name, so the
/// column count is not assumed: Node's table has eleven columns and io.js's
/// nine. Every other column of a surviving row is left byte-identical, and a
/// document whose first line is not a recognisable header is passed through
/// untouched — over-listing is the safe direction.
pub fn strip_index_tab(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let Some(tab) = IndexTab::parse(body) else {
        tracing::warn!("index.tab has no `version`/`date` header; passing through unfiltered");
        return Vec::new();
    };
    // The versions to drop, decided against the parsed rows; the rewrite below
    // walks the raw lines so nothing but whole blocked lines changes.
    let dropped: Vec<String> = tab
        .rows()
        .filter(|row| blocked.contains(row.version))
        .map(|row| row.version.to_owned())
        .collect();
    if dropped.is_empty() {
        return dropped;
    }

    let trailing_newline = body.ends_with('\n');
    let kept: Vec<&str> = body
        .lines()
        .enumerate()
        .filter(|(i, line)| {
            *i == 0 || {
                let version = line.split('\t').next().unwrap_or("").trim();
                !dropped.iter().any(|d| d == version)
            }
        })
        .map(|(_, line)| line)
        .collect();

    let mut out = kept.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    *body = out;
    dropped
}

/// Remove blocked entries from an `index.json` body — an array of objects,
/// each carrying a `version` field.
///
/// Anything that is not an array, and any element with no string `version`,
/// is left where it is: a document this proxy does not understand well enough
/// to edit is one it should not edit.
pub fn strip_index_json(doc: &mut Value, blocked: &BlockedVersions) -> Vec<String> {
    let Some(entries) = doc.as_array_mut() else {
        tracing::warn!("index.json is not an array; passing through unfiltered");
        return Vec::new();
    };
    let mut removed = Vec::new();
    entries.retain(|entry| match entry.get("version").and_then(Value::as_str) {
        Some(v) if blocked.contains(v) => {
            removed.push(v.to_owned());
            false
        }
        _ => true,
    });
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    /// Node's eleven-column table, newest first, as the tree serves it. The
    /// two `Jod` rows are what nvm's `lts/jod` alias is derived from.
    const INDEX_TAB: &str = "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
        v22.11.0\t2024-10-29\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t1.3.0.1-motley\t3.0.15+quic\t127\tJod\t-\n\
        v22.10.0\t2024-10-16\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t1.3.0.1-motley\t3.0.15+quic\t127\t-\t-\n\
        v22.9.0\t2024-09-17\theaders,linux-x64,src\t10.8.3\t12.4.254.21\t1.48.0\t1.3.0.1-motley\t3.0.15+quic\t127\tJod\t-\n\
        v20.18.0\t2024-10-03\theaders,linux-x64,src\t10.8.2\t11.3.244.8\t1.46.0\t1.3.0.1-motley\t3.0.13+quic\t115\tIron\ttrue\n";

    const IOJS_INDEX_TAB: &str = "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\n\
        v3.3.1\t2015-09-15\theaders,linux-x64,src\t2.14.3\t4.4.63.30\t1.7.4\t1.2.8\t1.0.2d\t45\n\
        v3.3.0\t2015-09-02\theaders,linux-x64,src\t2.13.3\t4.4.63.30\t1.7.3\t1.2.8\t1.0.2d\t45\n";

    fn blocked(versions: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Nodedist,
            versions.iter().map(|v| (*v).to_owned()).collect(),
        )
    }

    fn index_json() -> Value {
        serde_json::json!([
            { "version": "v22.11.0", "date": "2024-10-29", "files": ["linux-x64"], "lts": "Jod", "security": false },
            { "version": "v22.10.0", "date": "2024-10-16", "files": ["linux-x64"], "lts": false, "security": false },
            { "version": "v22.9.0",  "date": "2024-09-17", "files": ["linux-x64"], "lts": "Jod", "security": false },
            { "version": "v20.18.0", "date": "2024-10-03", "files": ["linux-x64"], "lts": "Iron", "security": true }
        ])
    }

    /// The case that matters most: nvm strips line 1 unconditionally, so a
    /// filter that dropped the header would make nvm eat the newest release.
    #[test]
    fn the_header_row_survives_filtering() {
        let mut body = INDEX_TAB.to_owned();
        let removed = strip_index_tab(&mut body, &blocked(&["v22.11.0"]));
        assert_eq!(removed, ["v22.11.0"]);
        assert!(
            body.starts_with("version\tdate\tfiles\t"),
            "line 1 must still be the header: {body:?}"
        );
        assert!(!body.contains("v22.11.0"));
    }

    #[test]
    fn a_blocked_row_is_removed_and_every_other_byte_is_kept() {
        let mut body = INDEX_TAB.to_owned();
        strip_index_tab(&mut body, &blocked(&["v22.10.0"]));
        let expected: String = INDEX_TAB
            .lines()
            .filter(|l| !l.starts_with("v22.10.0\t"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(body, expected);
    }

    /// Blocking the newest LTS moves the alias nvm derives from the `lts`
    /// column: the first surviving `Jod` row is now `v22.9.0`. Nothing here
    /// computes that — the point is that removal alone is enough.
    #[test]
    fn blocking_the_newest_lts_moves_the_alias_by_construction() {
        let mut body = INDEX_TAB.to_owned();
        strip_index_tab(&mut body, &blocked(&["v22.11.0"]));
        let first_jod = IndexTab::parse(&body)
            .unwrap()
            .rows()
            .find(|r| r.lts == Some("Jod"))
            .map(|r| r.version.to_owned());
        assert_eq!(first_jod.as_deref(), Some("v22.9.0"));
    }

    /// A block recorded without the `v` (the spelling `nvm install 22.11.0`
    /// takes) must still hide the prefixed row.
    #[test]
    fn a_block_without_the_v_prefix_matches_the_prefixed_row() {
        let mut body = INDEX_TAB.to_owned();
        let removed = strip_index_tab(&mut body, &blocked(&["22.11.0"]));
        assert_eq!(removed, ["v22.11.0"], "removed as the document spells it");
    }

    #[test]
    fn the_nine_column_iojs_table_is_filtered_without_touching_its_columns() {
        let mut body = IOJS_INDEX_TAB.to_owned();
        let removed = strip_index_tab(&mut body, &blocked(&["v3.3.0"]));
        assert_eq!(removed, ["v3.3.0"]);
        assert_eq!(
            body,
            "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\n\
             v3.3.1\t2015-09-15\theaders,linux-x64,src\t2.14.3\t4.4.63.30\t1.7.4\t1.2.8\t1.0.2d\t45\n"
        );
    }

    /// A block on a version the tree never had is a byte-identical passthrough,
    /// not a re-serialisation that happens to look the same.
    #[test]
    fn a_block_that_matches_nothing_leaves_the_document_byte_identical() {
        let mut body = INDEX_TAB.to_owned();
        assert!(strip_index_tab(&mut body, &blocked(&["v99.0.0"])).is_empty());
        assert_eq!(body, INDEX_TAB);
    }

    #[test]
    fn a_document_without_a_header_is_passed_through() {
        let mut body = "not\ta\ttable\n".to_owned();
        assert!(strip_index_tab(&mut body, &blocked(&["not"])).is_empty());
        assert_eq!(body, "not\ta\ttable\n");
    }

    #[test]
    fn index_json_drops_the_blocked_entry_and_keeps_the_rest() {
        let mut doc = index_json();
        let removed = strip_index_json(&mut doc, &blocked(&["22.10.0"]));
        assert_eq!(removed, ["v22.10.0"]);
        let versions: Vec<&str> = doc
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["version"].as_str().unwrap())
            .collect();
        assert_eq!(versions, ["v22.11.0", "v22.9.0", "v20.18.0"]);
        assert_eq!(doc[0]["lts"], "Jod", "the surviving entries are untouched");
    }

    #[test]
    fn index_json_that_is_not_an_array_is_left_alone() {
        let mut doc = serde_json::json!({ "version": "v22.11.0" });
        let before = doc.clone();
        assert!(strip_index_json(&mut doc, &blocked(&["v22.11.0"])).is_empty());
        assert_eq!(doc, before);
    }
}
