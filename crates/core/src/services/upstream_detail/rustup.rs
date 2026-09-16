//! The Rust toolchain tree: `manifests.txt`.
//!
//! One line per manifest the release tooling ever published, oldest first, with
//! the release date in the path. A line is worth a row for the same reason
//! Node's `index.tab` row is: the date it carries is the value the registry
//! client hands the age gate, so the console's version table and the gate
//! agree about when a release happened.
//!
//! Several lines describe one release — a beta has a dated `channel-rust-beta`
//! and up to three numbered spellings on the same day — so rows are
//! deduplicated on the coordinate, keeping the first date seen.

use super::{text, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;
use crate::services::rustup::{parse_release_date, ManifestsTxt};

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(body) = text(doc) else {
        return UpstreamDetail::default();
    };
    let parsed = ManifestsTxt::parse(body);
    if parsed.rows.is_empty() {
        tracing::warn!("manifests.txt names no manifest paths; contributing no rows");
        return UpstreamDetail::default();
    }

    let mut seen = std::collections::HashSet::new();
    let versions = parsed
        .rows
        .iter()
        .filter_map(|row| {
            let coordinate = row.coordinate.as_deref()?;
            if !seen.insert(coordinate.to_owned()) {
                return None;
            }
            Some(UpstreamVersion {
                published_at: parse_release_date(&row.date),
                ..UpstreamVersion::bare(coordinate)
            })
        })
        .collect();

    UpstreamDetail {
        versions,
        readmes: Default::default(),
        // Manifests and tarballs; the tree carries no prose and no links.
        links: None,
    }
}
