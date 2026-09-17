//! Ansible Galaxy: a collection's versions list (RFC 0031 §6.1).
//!
//! One page — this instance never serves more (RFC 0031 §4.4) — under a `data`
//! key, with `results` accepted as well because a standalone `pulp_ansible`
//! upstream spells it that way and the client itself accepts either.
//!
//! Each entry carries `created_at`, which is the same value the registry client
//! hands the age gate, so the page's version table and the gate agree on when a
//! collection version was published.

use super::{json, parse_time, UpstreamDetail, UpstreamVersion};
use crate::ports::VersionDocument;

pub(super) fn read(doc: &VersionDocument) -> UpstreamDetail {
    let Some(root) = json(doc) else {
        return UpstreamDetail::default();
    };
    let entries = root
        .get("data")
        .or_else(|| root.get("results"))
        .and_then(|v| v.as_array());
    let Some(entries) = entries else {
        return UpstreamDetail::default();
    };
    let versions = entries
        .iter()
        .filter_map(|entry| {
            let version = entry.get("version")?.as_str()?;
            Some(UpstreamVersion {
                published_at: entry
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .and_then(parse_time),
                ..UpstreamVersion::bare(version)
            })
        })
        .collect();
    UpstreamDetail {
        versions,
        readmes: Default::default(),
        // The versions list carries no prose and no repository link: both are
        // in `MANIFEST.json`, inside the tarball (RFC 0031 §6.1).
        links: None,
    }
}
