//! What the estate knows about an upstream's opinion of a cached package
//! (RFC 0014 §6.1).
//!
//! A row exists only while upstream has denied something: `present` is the
//! absence of a row. `version = None` is the whole package — every cached
//! version of the name went at once — and a version-level row is one
//! version. The two coexist for a name only transiently: a package-level
//! confirmation supersedes the version rows, and a successful probe of the
//! package clears both.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `last_error` is upstream-controlled text: stored verbatim, never parsed,
/// and cut here so a hostile upstream cannot use it as unbounded storage
/// (RFC 0014 §7).
pub const LAST_ERROR_MAX_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamState {
    /// Upstream denied it at least once; unconfirmed.
    Missing,
    /// Confirmed gone: `confirm_after` misses over at least
    /// `confirm_min_age_secs`, each in a valid sweep.
    Disappeared,
}

impl UpstreamState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Disappeared => "disappeared",
        }
    }
}

impl std::str::FromStr for UpstreamState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "missing" => Ok(Self::Missing),
            "disappeared" => Ok(Self::Disappeared),
            other => Err(format!("unknown upstream state '{other}'")),
        }
    }
}

impl fmt::Display for UpstreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of `upstream_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpstreamStatus {
    pub registry: String,
    pub package_name: String,
    /// `None` = the whole package is gone, not one version.
    pub version: Option<String>,
    pub state: UpstreamState,
    pub first_missed_at: DateTime<Utc>,
    pub last_checked_at: DateTime<Utc>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub consecutive_misses: u32,
    /// The upstream error as last seen, for the console. Never parsed.
    pub last_error: Option<String>,
}

impl UpstreamStatus {
    pub fn key(&self) -> UpstreamKey<'_> {
        UpstreamKey {
            registry: &self.registry,
            package_name: &self.package_name,
            version: self.version.as_deref(),
        }
    }

    /// The coordinate as `name` or `name@version`, the form the eviction
    /// hold and the reports use.
    pub fn coordinate(&self) -> String {
        hold_key(&self.package_name, self.version.as_deref())
    }
}

/// The set member for a held coordinate: `name` for a package-level row,
/// `name@version` for a version. A held version is looked up under both.
pub fn hold_key(package_name: &str, version: Option<&str>) -> String {
    match version {
        Some(v) => format!("{package_name}@{v}"),
        None => package_name.to_owned(),
    }
}

/// Addresses one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamKey<'a> {
    pub registry: &'a str,
    pub package_name: &'a str,
    /// `None` = the package-level row.
    pub version: Option<&'a str>,
}

impl<'a> UpstreamKey<'a> {
    pub fn package(registry: &'a str, package_name: &'a str) -> Self {
        Self {
            registry,
            package_name,
            version: None,
        }
    }

    pub fn version(registry: &'a str, package_name: &'a str, version: &'a str) -> Self {
        Self {
            registry,
            package_name,
            version: Some(version),
        }
    }
}

/// A miss, as the sweep observed it. Named fields rather than positional
/// arguments for the reason `ArtifactMetaRecord` has them: the adjacent
/// optional strings are easy to transpose.
#[derive(Debug, Clone, Copy)]
pub struct MissObservation<'a> {
    pub key: UpstreamKey<'a>,
    pub at: DateTime<Utc>,
    /// Truncated to [`LAST_ERROR_MAX_BYTES`] by the port implementation.
    pub error: Option<&'a str>,
}

/// What `list` and `count` select on.
#[derive(Debug, Clone, Default)]
pub struct UpstreamStatusFilter {
    pub registry: Option<String>,
    pub state: Option<UpstreamState>,
    /// `0` means no page size — everything.
    pub limit: usize,
    pub offset: usize,
}

/// Cut `last_error` to its stored size on a character boundary.
pub fn truncate_error(error: &str) -> String {
    if error.len() <= LAST_ERROR_MAX_BYTES {
        return error.to_owned();
    }
    let mut end = LAST_ERROR_MAX_BYTES;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}

/// What a confirmed disappearance does beyond the row (RFC 0014 §4.3):
/// `[upstream_audit] on_confirmed` for the estate, and — RFC 0014 §13 O6 —
/// a registry-tier policy row that overrides it (`[registries]
/// on_confirmed`, [`super::PolicyNode::on_confirmed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConfirmed {
    /// Record, hold, notify. The default, and the whole of phases 1–5.
    #[default]
    Audit,
    /// …and refuse the version on the wire through the admin block list
    /// (phase 6).
    Block,
}

impl OnConfirmed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::Block => "block",
        }
    }
}

impl std::str::FromStr for OnConfirmed {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "audit" => Ok(Self::Audit),
            "block" => Ok(Self::Block),
            other => Err(format!("unknown on_confirmed policy '{other}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        for s in [UpstreamState::Missing, UpstreamState::Disappeared] {
            assert_eq!(s.as_str().parse::<UpstreamState>().unwrap(), s);
        }
        assert!("gone".parse::<UpstreamState>().is_err());
    }

    #[test]
    fn hold_key_distinguishes_the_package_from_a_version() {
        assert_eq!(hold_key("left-pad", None), "left-pad");
        assert_eq!(hold_key("left-pad", Some("1.3.1")), "left-pad@1.3.1");
    }

    #[test]
    fn a_long_error_is_cut_on_a_character_boundary() {
        let long = "é".repeat(LAST_ERROR_MAX_BYTES);
        let cut = truncate_error(&long);
        assert!(cut.len() <= LAST_ERROR_MAX_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(truncate_error("short"), "short");
    }
}
