//! The scanner port (RFC 0018 §6.1): one trait every scanner speaks, so a
//! malware heuristic, a provenance check and the OSV lookup plug into the
//! same pipeline instead of each becoming one more gate with its own
//! failure mode.

use async_trait::async_trait;

use crate::entities::{Finding, PackageMetadata, RegistryKind};

/// Everything a scanner may look at.
pub struct ScanInput {
    pub package: PackageMetadata,
    /// The registry's kind — what a scanner's `supports` is asked about.
    pub kind: RegistryKind,
    /// The package URL of the coordinate, for the lookups that speak PURL.
    pub purl: String,
    /// The bytes, when the worker fetched them. Phase 1 scanners read
    /// metadata only; the archive scanners of phase 3 read this. Owned
    /// bytes rather than a stream so the input is `Sync` and every scanner
    /// can share it.
    pub artifact: Option<bytes::Bytes>,
    /// The CycloneDX SBOM already recorded for this artifact, when one is.
    pub sbom: Option<serde_json::Value>,
}

/// Why a scanner did not answer.
///
/// Distinct from "answered with nothing": under the default
/// `scanner_error = "quarantine"` an error holds the artifact, an empty
/// answer clears it. A scanner that swallowed its own error into an empty
/// list would be the `CveGateRule` fail-open all over again.
#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    #[error("scanner timed out")]
    Timeout,
    #[error("scanner could not reach its upstream: {0}")]
    Upstream(String),
    #[error("scanner output could not be read: {0}")]
    Output(String),
    #[error("scanner crashed: {0}")]
    Crashed(String),
    /// The scanner supports this ecosystem but could not build its input
    /// for this artifact (RFC 0018 §11 q1).
    #[error("scanner input unavailable: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

impl ScannerError {
    /// The `class` label of `batlehub_scanner_errors_total`.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Upstream(_) => "upstream",
            Self::Output(_) => "output",
            Self::Crashed(_) => "crashed",
            Self::Unsupported(_) => "unsupported",
            Self::Other(_) => "other",
        }
    }
}

/// One scanner.
#[async_trait]
pub trait ArtifactScanner: Send + Sync {
    /// The name `[scanners.<name>]` and `required_scanners` use.
    fn name(&self) -> &str;

    /// Whether this scanner has anything to say about artifacts of `kind`.
    /// A scanner declares its own coverage; a registry that lists it for a
    /// kind it does not cover is skipped there, not failed.
    fn supports(&self, kind: RegistryKind) -> bool;

    /// Scan. An empty `Ok` is a clean answer; an `Err` is not an answer.
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError>;
}
