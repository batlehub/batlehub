//! Go's release document: one entry per release, `go1.25.4`, no dates.

use super::{json, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;
use crate::services::godist::bare_version;

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let versions = json(doc)
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("version")?.as_str())
        .map(|v| UpstreamVersion::bare(bare_version(v)))
        .collect();
    UpstreamDetail {
        versions,
        readmes: Default::default(),
        // Archives and checksums; the document carries no prose and no links.
        links: None,
    }
}
