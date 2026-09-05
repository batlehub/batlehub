//! One package's probe (RFC 0014 §6.3): the cheapest useful question first,
//! and a capability gap is never evidence.
//!
//! The rung is chosen from `RegistryKind::upstream_detail()` — the enum
//! already declares whether a kind has a listing document, can enumerate
//! versions, or neither (0014 §13) — rather than inferred from an empty
//! `Vec`, which is what the default `list_versions` returns for a kind that
//! never implemented it.

use crate::entities::{PackageId, RegistryKind, UpstreamDetailSupport};
use crate::error::CoreError;
use crate::ports::RegistryClient;

/// Rung 3 is one request per version, so a package that reaches it is
/// covered over several sweeps rather than in one burst of hundreds.
pub const MAX_VERSION_PROBES_PER_PACKAGE: usize = 25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Upstream confirms every cached version.
    Present,
    /// Upstream denies the package itself.
    MissingPackage,
    /// Upstream lists the package but not these cached versions.
    MissingVersions(Vec<String>),
    /// Upstream could not answer: a capability gap, an error, a timeout.
    /// Excluded from both sides of the ratio; the string is for the report.
    Inconclusive(Option<String>),
}

impl ProbeOutcome {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::MissingPackage | Self::MissingVersions(_))
    }
}

/// Whether `kind` answers rung 1 — one listing request covers every cached
/// version — or falls straight to the per-version rung 3.
pub fn listing_capable(kind: RegistryKind) -> bool {
    matches!(
        kind.upstream_detail(),
        UpstreamDetailSupport::Document(_) | UpstreamDetailSupport::ListVersions
    )
}

/// The rung a miss was observed on, as the notification names it (RFC 0014
/// §4.5 `probe`).
pub fn probe_name(kind: RegistryKind, outcome: &ProbeOutcome) -> &'static str {
    match outcome {
        ProbeOutcome::MissingPackage => "package",
        ProbeOutcome::MissingVersions(_) if listing_capable(kind) => "version_listing",
        ProbeOutcome::MissingVersions(_) => "per_version",
        ProbeOutcome::Present | ProbeOutcome::Inconclusive(_) => "none",
    }
}

/// Probe `name`'s cached `versions` against `client`.
///
/// Returns the outcome and whether the per-version cap bound the probe.
pub async fn probe_package(
    client: &dyn RegistryClient,
    kind: RegistryKind,
    registry: &str,
    name: &str,
    versions: &[String],
) -> (ProbeOutcome, bool) {
    if listing_capable(kind) {
        // Rung 1: one request covers every cached version.
        match client.list_versions(name).await {
            Ok(listed) if !listed.is_empty() => {
                let missing: Vec<String> = versions
                    .iter()
                    .filter(|v| !listed.iter().any(|l| l == *v))
                    .cloned()
                    .collect();
                return if missing.is_empty() {
                    (ProbeOutcome::Present, false)
                } else {
                    (ProbeOutcome::MissingVersions(missing), false)
                };
            }
            // Rung 2: empty is inconclusive — fall through to rung 3.
            Ok(_) => {}
            // Rung 4: upstream denies the package itself.
            Err(CoreError::NotFound(_)) | Err(CoreError::NotFoundWithheld(_)) => {
                return (ProbeOutcome::MissingPackage, false);
            }
            // Rung 5: an upstream that failed to answer has not denied
            // anything.
            Err(e) => return (ProbeOutcome::Inconclusive(Some(e.to_string())), false),
        }
    }

    // Rung 3: per version, oldest-cached first (the inventory's order),
    // capped.
    let capped = versions.len() > MAX_VERSION_PROBES_PER_PACKAGE;
    let mut missing = Vec::new();
    let mut answered = 0usize;
    for v in versions.iter().take(MAX_VERSION_PROBES_PER_PACKAGE) {
        let id = PackageId::new(registry, name, v);
        match client.resolve_metadata(&id).await {
            Ok(_) => answered += 1,
            Err(CoreError::NotFound(_)) | Err(CoreError::NotFoundWithheld(_)) => {
                answered += 1;
                missing.push(v.clone());
            }
            Err(e) => {
                // Stop on the first failure to answer: the remaining
                // versions would be asked of an upstream that is not
                // answering, and one inconclusive package is the honest
                // result.
                return (ProbeOutcome::Inconclusive(Some(e.to_string())), capped);
            }
        }
    }
    if answered == 0 {
        return (ProbeOutcome::Inconclusive(None), capped);
    }
    if missing.is_empty() {
        (ProbeOutcome::Present, capped)
    } else {
        (ProbeOutcome::MissingVersions(missing), capped)
    }
}
