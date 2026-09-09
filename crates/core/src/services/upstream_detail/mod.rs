//! Reading an upstream's own listing document for the console's discovery read
//! (RFC 0007 §5.5).
//!
//! Laid out like [`crate::services::blocking`], and for the same reasons: one
//! file per protocol, each a **pure function** over a `VersionDocument`, one
//! `dispatch` call site, and coverage declared by an exhaustive match on
//! `RegistryKind`. `blocking` already proves `core` can read these documents
//! without knowing what HTTP is.
//!
//! Two modules parsing the same documents to different structs would drift, so
//! this reads what both halves of the page need — the version list *and*, for
//! the kinds whose listing carries it, the README — and `blocking`'s filters
//! stay where they are.
//!
//! **Everything here fails open in the safe direction**, which for a read path
//! means *fewer rows* rather than a broken page: an unparseable document is
//! warned about and contributes nothing, a missing field yields `None` rather
//! than a guess, and no failure of this path can make an artifact retrievable —
//! the download gate re-checks the concrete coordinate as it always has.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::entities::{MetadataLinks, MetadataReadme, RegistryKind};
use crate::ports::VersionDocument;

pub use coordinator::UpstreamDetailCoordinator;

mod cargo;
mod composer;
pub mod coordinator;
mod goproxy;
mod maven;
mod nodedist;
mod npm;
mod nuget;
mod pypi;
mod rubygems;
mod sdkman;
mod terraform;

/// One version an upstream knows about, described only by what its listing
/// document actually says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpstreamVersion {
    pub version: String,
    /// When the upstream published it, when the document carries that. `None`
    /// where the protocol has no such field — a NuGet flat index is a list of
    /// strings — rather than a guess.
    pub published_at: Option<DateTime<Utc>>,
    pub is_prerelease: bool,
    /// The upstream's own withdrawal mark, where the protocol has one: cargo's
    /// `yanked`. **Not this instance's policy** — blocks are applied on top, by
    /// the handler, so the two can be told apart on the page.
    pub yanked: bool,
    /// The upstream's own deprecation notice, where the protocol has one: npm's
    /// `deprecated` string.
    pub deprecated: Option<String>,
}

impl UpstreamVersion {
    /// A version with nothing known about it but its number.
    ///
    /// The honest shape for the protocols whose listing is a bare list, and the
    /// reason the page renders those cells as *unknown* rather than as `0`.
    pub fn bare(version: impl Into<String>) -> Self {
        let version = version.into();
        Self {
            is_prerelease: is_prerelease(&version),
            version,
            published_at: None,
            yanked: false,
            deprecated: None,
        }
    }
}

/// What one upstream listing document says about a package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpstreamDetail {
    pub versions: Vec<UpstreamVersion>,
    /// Per-version READMEs, for the kinds whose listing document carries the
    /// text: npm's packument does, a cargo sparse index does not.
    pub readmes: HashMap<String, MetadataReadme>,
    /// The package's own links, for the kinds whose listing document carries
    /// them — npm's packument and Composer's p2 metadata, and no others.
    ///
    /// **Package-level, not per-version.** The precise answer is the selected
    /// version's own entry in the metadata cache, and the page prefers it when
    /// it is there; this is the answer that is *always* there, because the page
    /// has already read this document to build its version table. Without it the
    /// link depended on a `meta:` entry that no request the page itself makes
    /// ever writes for npm — and that expires after `metadata_ttl` for the kinds
    /// where the README panel happens to write it, so the block appeared or not
    /// depending on what had been read in the last five minutes.
    ///
    /// `None` for the kinds whose listing genuinely says nothing about a
    /// repository: a cargo sparse index, a NuGet flat index, a PEP 691 simple
    /// page, `maven-metadata.xml`, `@v/list`, the RubyGems versions array and
    /// the Terraform version lists all carry versions and nothing else. Those
    /// keep answering from the metadata cache alone.
    pub links: Option<MetadataLinks>,
}

/// Read `doc` as `kind`'s listing document.
///
/// The single call site. A kind with no reader here returns an empty detail and
/// warns — the page then answers from local rows, which is the degradation this
/// whole path is designed around.
pub fn dispatch(kind: RegistryKind, doc: &VersionDocument) -> UpstreamDetail {
    match kind {
        RegistryKind::Npm => npm::read(doc),
        RegistryKind::Pypi => pypi::read(doc),
        RegistryKind::Cargo => cargo::read(doc),
        RegistryKind::Nuget => nuget::read(doc),
        RegistryKind::Goproxy => goproxy::read(doc),
        RegistryKind::Maven => maven::read(doc),
        RegistryKind::Composer => composer::read(doc),
        RegistryKind::Rubygems => rubygems::read(doc),
        RegistryKind::Terraform => terraform::read(doc),
        RegistryKind::Nodedist => nodedist::read(doc),
        RegistryKind::Sdkman => sdkman::read(doc),
        other => {
            // Reachable only through a bug: `RegistryKind::upstream_detail()`
            // answers `Document(_)` for exactly the kinds above, and the drift
            // test below refuses any other combination. Warned rather than
            // panicked, because this is a page-render path.
            tracing::warn!(
                kind = %other,
                "no upstream-detail reader for this registry kind; answering from local rows"
            );
            UpstreamDetail::default()
        }
    }
}

/// Whether this kind's *listing* document is also where its README lives.
///
/// npm's packument is both: it lists the versions and carries the text, so if
/// the listing read found none, resolving a version would re-fetch the same
/// document and find the same nothing. PyPI's simple page lists versions and
/// carries no description at all, and the extension galleries answer by query —
/// for those, a per-version resolve is a different request with a different
/// answer, and worth making.
///
/// Exhaustive with no wildcard arm, so a kind added to `dispatch` has to say
/// which it is rather than silently costing an extra upstream request per
/// version viewed.
pub fn listing_carries_readmes(kind: RegistryKind) -> bool {
    match kind {
        RegistryKind::Npm => true,
        RegistryKind::Pypi
        | RegistryKind::Cargo
        | RegistryKind::Nuget
        | RegistryKind::Goproxy
        | RegistryKind::Maven
        | RegistryKind::Composer
        | RegistryKind::Rubygems
        | RegistryKind::Terraform
        | RegistryKind::Conda
        | RegistryKind::Openvsx
        | RegistryKind::VscodeMarketplace
        | RegistryKind::JetbrainsMarketplace
        | RegistryKind::Github
        | RegistryKind::Gitlab
        | RegistryKind::Forgejo
        | RegistryKind::Deb
        | RegistryKind::Rpm
        | RegistryKind::Pacman
        | RegistryKind::Jetbrains
        | RegistryKind::Generic
        | RegistryKind::Nodedist
        | RegistryKind::Sdkman => false,
    }
}

/// Whether this kind's *listing* document is also where its links live.
///
/// The sibling of [`listing_carries_readmes`], asked for the same reason and
/// answered by the same rule: a `true` here means the page has already seen
/// everything the upstream will say about the repository, so a package that
/// declared none is *known* to have declared none and a per-version resolve
/// would re-read the document it just read.
///
/// A `false` means the answer is in a document the page has not fetched — the
/// crates.io API record, `/pypi/{name}/{version}/json`, the POM, the gallery's
/// per-extension query — and is worth one resolve for the **one version
/// selected**, cached afterwards for the registry's `metadata_ttl`.
///
/// The cost of `true` on a kind whose listing is incomplete: an npm package that
/// declared a repository on an *older* version and none at the root shows no
/// link. That is accepted — the alternative is an upstream request on every page
/// view of every package that legitimately declares nothing, which is the
/// common case and the one this predicate exists to keep free.
///
/// Exhaustive with no wildcard arm, for the same reason as its sibling.
pub fn listing_carries_links(kind: RegistryKind) -> bool {
    match kind {
        // The packument's root and its `dist-tags.latest` entry, and Composer's
        // p2 entries — read by `npm::read` and `composer::read` into
        // [`UpstreamDetail::links`].
        RegistryKind::Npm | RegistryKind::Composer => true,
        // Version lists and nothing else. See each reader's own `links: None`
        // for which document does carry the answer.
        RegistryKind::Pypi
        | RegistryKind::Cargo
        | RegistryKind::Nuget
        | RegistryKind::Goproxy
        | RegistryKind::Maven
        | RegistryKind::Rubygems
        | RegistryKind::Terraform
        | RegistryKind::Conda
        | RegistryKind::Openvsx
        | RegistryKind::VscodeMarketplace
        | RegistryKind::JetbrainsMarketplace
        | RegistryKind::Github
        | RegistryKind::Gitlab
        | RegistryKind::Forgejo
        | RegistryKind::Deb
        | RegistryKind::Rpm
        | RegistryKind::Pacman
        | RegistryKind::Jetbrains
        | RegistryKind::Generic
        | RegistryKind::Nodedist
        | RegistryKind::Sdkman => false,
    }
}

/// RFC 0015 §4.5's one definition of "pre-release", for the upstream half of
/// the detail page's version table.
///
/// This *was* the crude `contains('-')` rule, and its doc comment defended the
/// crudeness with the table's own consistency: two lists in one table, and a row
/// graded differently depending on which list it came from would be a visible
/// inconsistency. The argument was sound and the conclusion was not — one shared
/// function serves that consistency better than two matching approximations, and
/// the local half of the same table now calls the same thing.
///
/// The re-export is kept rather than replaced at each call site because five
/// ecosystem parsers in this module name it, and one of them
/// (`rubygems.rs`) uses it only as a fallback behind the upstream's own
/// `prerelease` flag.
pub(crate) use crate::services::version_order::is_prerelease;

/// Parse an RFC 3339 timestamp, ignoring anything that is not one.
pub(crate) fn parse_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// The JSON body, or a warning and nothing.
pub(crate) fn json(doc: &VersionDocument) -> Option<&serde_json::Value> {
    let body = doc.body.as_json();
    if body.is_none() {
        tracing::warn!("upstream detail expected a JSON document and got text");
    }
    body
}

/// The text body, or a warning and nothing.
pub(crate) fn text(doc: &VersionDocument) -> Option<&str> {
    let body = doc.body.as_text();
    if body.is_none() {
        tracing::warn!("upstream detail expected a text document and got JSON");
    }
    body
}

#[cfg(test)]
mod tests;
