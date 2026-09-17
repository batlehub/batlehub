//! The Nix binary-cache (substituter) protocol as a registry kind (RFC 0028).
//!
//! A directory rather than a file because three concerns that do not belong
//! together each need room: the upstream reads ([`client`]), the reverse index
//! that lets a client's *stale* NAR URL still resolve to a coordinate
//! ([`reverse`]), and the upload verification that earns the registry's own
//! signature ([`verify`], phase 4).
//!
//! The protocol itself — the grammars, the fingerprint, the signing key — is in
//! `batlehub_core::services::nix`, because none of it does I/O and all of it is
//! a fact about Nix rather than about this adapter.

mod client;
mod reverse;
mod verify;

pub use client::{content_type_for, local_cache_info, nar_content_type, NixBinaryCacheClient};
pub use reverse::{lookup_nar_url, nar_artifact, nar_url_index_key, remember_nar_url};
pub use verify::{check_nar, SUPPORTED_COMPRESSION};

#[cfg(test)]
mod tests;
