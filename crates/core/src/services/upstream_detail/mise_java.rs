//! A `mise-java` metadata document: one row per build, each dated
//! (`created_at`), keyed by the coordinate mise builds for it.

use super::{json, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;
use crate::services::mise_java::{coordinate, created_at, Row};

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let mut versions: Vec<UpstreamVersion> = json(doc)
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value::<Row>(v.clone()).ok())
        .map(|r| UpstreamVersion {
            published_at: created_at(&r),
            ..UpstreamVersion::bare(coordinate(&r))
        })
        .collect();
    // A release has one row per archive format and image; one line each.
    versions.sort_by(|a, b| a.version.cmp(&b.version));
    versions.dedup_by(|a, b| a.version == b.version);
    UpstreamDetail {
        versions,
        readmes: Default::default(),
        // Archives named by URL; the document carries no prose and no links.
        links: None,
    }
}
