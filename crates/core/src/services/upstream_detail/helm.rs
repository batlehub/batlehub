//! Helm chart repositories: one chart's entries of `index.yaml` (RFC 0029 §6.1).
//!
//! The adapter answers a chart name with an index holding that chart alone, so
//! the page reads the document shape the index filter reads. `created` is the
//! date the age gate reads too; `deprecated: true` is the chart's own mark.

use super::{json, parse_time, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(entries) = json(doc)
        .and_then(|root| root.get("entries"))
        .and_then(|e| e.as_object())
    else {
        return UpstreamDetail::default();
    };
    let versions = entries
        .values()
        .filter_map(|v| v.as_array())
        .flatten()
        .filter_map(|entry| {
            let version = entry.get("version")?.as_str()?;
            Some(UpstreamVersion {
                published_at: entry
                    .get("created")
                    .and_then(|v| v.as_str())
                    .and_then(parse_time),
                deprecated: entry
                    .get("deprecated")
                    .and_then(|v| v.as_bool())
                    .filter(|d| *d)
                    .map(|_| "marked deprecated in its Chart.yaml".to_owned()),
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
    fn versions_dates_and_deprecation_are_read() {
        let doc = VersionDocument::json(serde_json::json!({"entries": {"x": [
            {"version": "1.1.0", "created": "2026-09-11T12:39:49.209345931Z"},
            {"version": "1.0.0-rc.1", "deprecated": true}
        ]}}));
        let d = read(&doc);
        assert_eq!(d.versions.len(), 2);
        assert!(d.versions[0].published_at.is_some());
        assert!(d.versions[1].is_prerelease);
        assert!(d.versions[1].deprecated.is_some());
    }
}
