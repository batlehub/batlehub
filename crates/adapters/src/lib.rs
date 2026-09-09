pub mod cache;
pub mod in_memory;
#[cfg(feature = "registry-cargo")]
pub mod listing_facts;
pub mod migrations;
pub mod notification;
pub mod rate_limit;
pub mod sbom;
pub mod scanners;

#[cfg(any(
    feature = "auth-token",
    feature = "auth-oidc",
    feature = "auth-kubernetes",
    feature = "auth-actions-oidc"
))]
pub mod auth;

#[cfg(any(feature = "storage-fs", feature = "storage-s3"))]
pub mod storage;

pub mod registry;

/// Debian APT / RPM / Arch pacman repository hosting: package parsing, index
/// generation, and Ed25519 OpenPGP signing. Gated on the deb/rpm/pacman registry
/// features.
#[cfg(any(
    feature = "registry-deb",
    feature = "registry-rpm",
    feature = "registry-pacman"
))]
pub mod repo;

#[cfg(feature = "vuln-scan")]
pub mod vulnerability;

#[cfg(feature = "db-postgres")]
pub mod db;

#[cfg(feature = "local-registry")]
pub mod local_registry;
