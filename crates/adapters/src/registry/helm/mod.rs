//! Classic Helm chart repositories (RFC 0029).
//!
//! A directory because two concerns share the protocol: the upstream reads —
//! `index.yaml`, the archives and their `.prov` files, resolved the way `helm`
//! resolves them ([`client`]) — and the publish reader that takes `Chart.yaml`
//! out of an uploaded archive without extracting it ([`archive`]).

pub mod archive;
pub mod client;

#[cfg(test)]
mod tests;

pub use archive::chart_yaml_from_archive;
pub use client::{resolve_reference, HelmRegistryClient};
