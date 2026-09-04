//! `ArtifactScanner` implementations (RFC 0018 §6.3).
//!
//! Phase 1 shipped `osv`. Phase 3 adds the archive scanners — `postmortem`,
//! `guarddog`, `trivy` — behind the [`subprocess`] runner (`bwrap`) and the
//! [`extract`] policy, and `sigstore` for provenance. The external services
//! (`socket`, `mlab`) are phase 5.

pub mod osv;
pub mod sigstore;
pub mod subprocess;

#[cfg(feature = "sbom")]
pub mod extract;
#[cfg(feature = "sbom")]
pub mod guarddog;
#[cfg(feature = "sbom")]
pub mod postmortem;
#[cfg(feature = "sbom")]
pub mod trivy;

pub use osv::OsvArtifactScanner;
pub use sigstore::SigstoreScanner;
pub use subprocess::Sandbox;

#[cfg(feature = "sbom")]
pub use extract::ExtractPolicy;
#[cfg(feature = "sbom")]
pub use guarddog::GuarddogScanner;
#[cfg(feature = "sbom")]
pub use postmortem::PostmortemScanner;
#[cfg(feature = "sbom")]
pub use trivy::TrivyScanner;
