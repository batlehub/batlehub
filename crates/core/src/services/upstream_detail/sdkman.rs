//! SDKMAN: `candidates/{c}/{plat}/versions/all`.
//!
//! A comma-separated list of identifiers and nothing else. SDKMAN publishes
//! no dates, so every row is bare — which is also why, on this kind, the age
//! gate's `deny_missing_timestamp` is the whole rule (RFC 0010 §6.7).

use super::{text, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;
use crate::services::sdkman::versions_in_csv;

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(body) = text(doc) else {
        return UpstreamDetail::default();
    };
    let versions = versions_in_csv(body)
        .into_iter()
        .map(|v| UpstreamVersion::bare(&v))
        .collect();
    UpstreamDetail {
        versions,
        readmes: Default::default(),
        // Identifiers; the protocol carries no prose and no links.
        links: None,
    }
}
