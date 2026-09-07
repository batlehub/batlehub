//! What a forge release looks like once the three forges agree (RFC 0021 §5.2).
//!
//! `RegistryClient` is enough to *fetch* a release asset — the coordinate is
//! `{repo}@{tag}` with the asset as the sub-coordinate, and every byte goes out
//! through the client's own credential and SSRF guard. It is not enough to
//! *choose* one: `resolve_metadata` hands back each forge's own JSON shape
//! (GitHub lists `assets[]` addressed `filename/<name>`, GitLab lists
//! `assets.links[]` addressed `link/<name>`), and a caller that parsed those
//! would be three forge-specific parsers wearing a trench coat.
//!
//! So the normalisation lives here, next to the clients that know their own
//! JSON, and the import service above it never learns which forge answered.

use async_trait::async_trait;

use crate::error::CoreError;

/// One asset attached to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeAsset {
    /// The file name, as the release page shows it — what an asset glob matches
    /// and what a filename-derived coordinate is read from.
    pub name: String,
    /// The `PackageId::artifact` that addresses these bytes through
    /// [`RegistryClient::fetch_artifact`], in this forge's own spelling
    /// (`filename/x.vsix`, `link/x.vsix`). Opaque above this port: it is built
    /// by the client that will be handed it back.
    ///
    /// [`RegistryClient::fetch_artifact`]: super::RegistryClient::fetch_artifact
    pub artifact: String,
    /// Size in bytes when the forge reports one. Advisory: it is the forge's
    /// number, not a measurement, so nothing decides on it that the bytes
    /// themselves could answer.
    pub size: Option<u64>,
}

/// One release, in the only three facts an import needs to choose between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeRelease {
    pub tag: String,
    /// Not published on the forge. Never imported, by any setting (RFC 0021
    /// §4.2): importing one would serve bytes the producing team has not
    /// released.
    pub draft: bool,
    /// Published and marked as not the default download. Imported by tag, or
    /// under `releases = "all"`, and never by `latest`.
    pub prerelease: bool,
    pub assets: Vec<ForgeAsset>,
}

impl ForgeRelease {
    /// Whether `latest` may choose this release.
    pub fn is_stable(&self) -> bool {
        !self.draft && !self.prerelease
    }
}

/// The releases of one repository, normalised across forges.
///
/// Implemented by the `github`, `gitlab` and `forgejo` clients; nothing else
/// serves releases, and `AppConfig::validate()` refuses a `from` that is not one
/// of the three rather than letting an import discover it at run time.
#[async_trait]
pub trait ForgeReleaseSource: Send + Sync {
    /// Newest first, as the forge orders them.
    ///
    /// Bounded by whatever page the forge answers with: this is how `latest` is
    /// chosen, not an inventory.
    async fn list_releases(&self, repo: &str) -> Result<Vec<ForgeRelease>, CoreError>;

    /// One release by its tag.
    ///
    /// Separate from [`Self::list_releases`] because a tag older than the
    /// forge's first page exists and is not in that page — asking for it by
    /// name is one request, and finding it by paging is many.
    async fn release_by_tag(&self, repo: &str, tag: &str) -> Result<ForgeRelease, CoreError>;
}
