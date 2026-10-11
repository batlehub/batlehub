//! Devfile registries: one stack's entry of the v2 index (RFC 0035 §6.1).
//!
//! The adapter answers a stack name with that stack's entry alone, so the page
//! reads the same document shape the index filter reads.
//!
//! No date is read: `lastModified` is the time upstream last rebuilt the whole
//! registry — on 2026-09-22, 87 of 90 versions carried the same instant and the
//! other 3 Go's zero time — so it would put one false date on every row. The
//! `Deprecated` tag is how upstream marks a version it no longer recommends.

use super::{json, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(entry) = json(doc) else {
        return UpstreamDetail::default();
    };
    let Some(records) = entry.get("versions").and_then(|v| v.as_array()) else {
        return UpstreamDetail::default();
    };
    let versions = records
        .iter()
        .filter_map(|record| {
            let version = record.get("version")?.as_str()?;
            let deprecated = record
                .get("tags")
                .and_then(|t| t.as_array())
                .is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some("Deprecated")))
                .then(|| "tagged Deprecated in the devfile registry".to_owned());
            Some(UpstreamVersion {
                deprecated,
                ..UpstreamVersion::bare(version)
            })
        })
        .collect();
    UpstreamDetail {
        versions,
        readmes: Default::default(),
        links: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_the_deprecated_tag_are_read() {
        let doc = VersionDocument::json(serde_json::json!({
            "name": "nodejs",
            "versions": [
                {"version": "2.2.1", "default": true, "tags": ["Node.js"]},
                {"version": "2.1.1", "tags": ["Node.js", "Deprecated"]}
            ]
        }));
        let detail = read(&doc);
        assert_eq!(detail.versions.len(), 2);
        assert_eq!(detail.versions[0].version, "2.2.1");
        assert!(detail.versions[0].deprecated.is_none());
        assert!(detail.versions[1].deprecated.is_some());
    }
}
