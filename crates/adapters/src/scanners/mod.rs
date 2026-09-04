//! `ArtifactScanner` implementations (RFC 0018 §6.3). Phase 1 ships `osv`;
//! the archive scanners behind the `bwrap` runner and the external services
//! arrive with phases 3 and 5.

pub mod osv;

pub use osv::OsvArtifactScanner;
