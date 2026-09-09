//! The `nodejs.org/dist` tree: `index.tab`.
//!
//! One row per release, newest first, with the release date in the second
//! column and — on Node's table, not io.js's — the LTS codename in the `lts`
//! column. The date is what makes this row worth more than a bare version:
//! it is the same value the registry client hands the age gate, so the page's
//! version table and the gate agree on when a release happened.

use super::{text, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;
use crate::services::nodedist::{parse_index_date, IndexTab};

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(body) = text(doc) else {
        return UpstreamDetail::default();
    };
    let Some(tab) = IndexTab::parse(body) else {
        tracing::warn!("index.tab has no recognisable header; contributing no rows");
        return UpstreamDetail::default();
    };
    let versions = tab
        .rows()
        .map(|row| UpstreamVersion {
            published_at: parse_index_date(row.date),
            ..UpstreamVersion::bare(row.version)
        })
        .collect();
    UpstreamDetail {
        versions,
        readmes: Default::default(),
        // Tarballs and checksums; the tree carries no prose and no links.
        links: None,
    }
}
