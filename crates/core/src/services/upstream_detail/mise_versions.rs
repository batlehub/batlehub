//! mise's versions host: `data/<tool>.toml` (RFC 0043 §6.1).
//!
//! Every entry carries `created_at`, the same date the listing's age gate reads,
//! so the page's version table and the gate agree on when a version appeared.

use super::{text, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;
use crate::services::mise_versions::versions;

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(body) = text(doc) else {
        return UpstreamDetail::default();
    };
    UpstreamDetail {
        versions: versions(body)
            .into_iter()
            .map(|(v, at)| UpstreamVersion {
                published_at: at,
                ..UpstreamVersion::bare(&v)
            })
            .collect(),
        readmes: Default::default(),
        // A version list; `release_url` names a release page, not the project.
        links: None,
    }
}
