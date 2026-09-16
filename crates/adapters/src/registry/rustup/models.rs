//! The one document in this tree with a DTO.
//!
//! `rustup/release-stable.toml` is forty bytes naming the installer's current
//! version, and the bootstrap route has to read it to give `rustup-init` a
//! coordinate that is a version rather than a moving name (RFC 0024 §4.3).
//!
//! The channel manifest deliberately has none: it is 900 KB, the read path
//! needs four fields out of it, and building a value tree would end the
//! byte-exact property its `.asc` and its sidecar depend on (§6.2). It is
//! indexed by section in `batlehub_core::services::rustup::Manifest` instead.

use serde::Deserialize;

/// `rustup/release-stable.toml`, in full:
///
/// ```toml
/// schema-version = "1"
/// version = "1.29.1"
/// ```
///
/// `schema-version` is deliberately not a field: nothing here branches on it,
/// and a struct member nobody reads is dead code the lint gate rejects. A
/// future schema that moved `version` would fail this deserialise, which is
/// the honest failure and the one an operator can act on.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseStable {
    pub version: String,
}
