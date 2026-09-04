//! What an air-gapped instance was asked for and did not hold (RFC 0008
//! §4.4, §13 decision 3).
//!
//! The record is the point of the mode. A disconnected instance that only
//! fails is a mystery; one that says *what* it lacks turns the next bundle
//! into a list the estate produced rather than a guess an operator made.
//!
//! One row per `(registry, storage_key)` with a counter, because mise
//! retries and the log must not grow with the retries — and capped per
//! registry, because the key is attacker-writable: anyone who can ask this
//! instance for a package can write a row.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What kind of thing was missing. One table, one column — the same shape
/// RFC 0018 uses for its single verdict table with reason codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MissKind {
    /// The bytes of a version.
    Artifact,
    /// A listing or metadata document.
    Document,
    /// A checksum or signature file the client fetches beside an artifact.
    Checksum,
    /// A forge ref the bundle did not carry a resolution for (RFC 0019).
    Ref,
    /// A host nothing rewrites — reached through the catch-all rule, which
    /// answers `501` and records this (RFC 0008 §4.4).
    UnmirroredHost,
}

impl MissKind {
    pub const ALL: [MissKind; 5] = [
        Self::Artifact,
        Self::Document,
        Self::Checksum,
        Self::Ref,
        Self::UnmirroredHost,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::Document => "document",
            Self::Checksum => "checksum",
            Self::Ref => "ref",
            Self::UnmirroredHost => "unmirrored_host",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One thing this instance was asked for and did not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ContentMiss {
    pub registry: String,
    /// The storage key it would occupy — the same key a bundle entry names,
    /// so the record is a statement about this instance's storage rather
    /// than about a URL.
    pub storage_key: String,
    pub kind: MissKind,
    /// The coordinate as the client spelled it, for a human reading the
    /// list. Never parsed; it is a label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<String>,
}

/// A recorded miss, as the admin surface and the next plan read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecordedMiss {
    pub registry: String,
    pub storage_key: String,
    pub kind: MissKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// How many times it was asked for. mise retries; this is the number
    /// that tells an operator which gap actually hurts.
    pub count: u64,
}

/// Which recorded misses to list.
#[derive(Debug, Clone, Default)]
pub struct MissFilter {
    pub registry: Option<String>,
    pub kind: Option<MissKind>,
    pub limit: u64,
    pub offset: u64,
}

/// The air-gap policy the read path reads, snapshotted from the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirGapPolicy {
    pub enabled: bool,
    pub record_misses: bool,
    pub miss_retention_days: u32,
    /// The keys an imported bundle's signature must verify against.
    pub bundle_trusted_keys: Vec<String>,
}

impl Default for AirGapPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            record_misses: true,
            miss_retention_days: 90,
            bundle_trusted_keys: Vec::new(),
        }
    }
}

/// The most rows one registry may hold. The key is attacker-writable — a
/// caller who can ask for a package can write a row — so the table is capped
/// and the oldest-seen rows go first (RFC 0008 §13 decision 3).
pub const MAX_MISSES_PER_REGISTRY: u64 = 10_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_its_wire_spelling() {
        for k in MissKind::ALL {
            assert_eq!(MissKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(MissKind::parse("nope"), None);
        // The one with an underscore, spelled out: a `_` in the wire form and
        // a camel-case variant is exactly where a hand-written table drifts.
        assert_eq!(MissKind::UnmirroredHost.as_str(), "unmirrored_host");
    }

    #[test]
    fn the_default_policy_is_todays_behaviour() {
        let p = AirGapPolicy::default();
        assert!(!p.enabled);
        assert!(p.record_misses);
    }
}

/// One accepted bundle, as the admin surface reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BundleImport {
    pub bundle_id: String,
    /// The public key whose signature verified. Naming it is how an operator
    /// tells two signers apart; it is the public half and not a secret.
    pub signer_key: String,
    pub imported_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<String>,
    pub entries: u64,
    pub blobs: u64,
    pub rejected: u64,
    /// A few of what was rejected, for the line an operator reads before
    /// deciding whether to care.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_sample: Option<String>,
    /// The plan the bundle was built from, when the builder recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_from: Option<String>,
}
