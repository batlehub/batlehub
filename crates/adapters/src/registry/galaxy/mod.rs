//! Ansible Galaxy (RFC 0031).
//!
//! A directory rather than a flat file because four concerns share one
//! protocol: discovery and the collection reads ([`client`]), the page walk
//! that keeps every served listing to one page ([`pagination`]), the upstream
//! response shapes ([`models`]), and the publish reader that opens a collection
//! tarball without extracting it ([`publish`]).

pub mod client;
pub mod models;
pub mod pagination;
pub mod publish;

#[cfg(test)]
mod tests;

pub use client::{GalaxyRegistryClient, ROLE_DOWNLOAD_HOSTS};
pub use publish::{read_collection_tarball, PublishedCollection};

/// The API root a `galaxy` registry defaults to when `upstreams` is absent.
pub const DEFAULT_API_BASE: &str = "https://galaxy.ansible.com/api/";
