//! A package version's own account of itself.
//!
//! Keyed by `(registry, name, version)` from the start, because a README
//! describes the code that shipped with it: a package whose 2.x README documents
//! an API the 1.x README does not is the normal case, not an edge case (RFC 0007
//! §2.4). Retrofitting a version key onto a package-level column later means
//! either a migration that cannot backfill or a page that lies.
//!
//! The store keeps the **source**, never the rendered HTML. A fix to the
//! sanitiser then applies to everything already stored — the render cache is
//! keyed by content digest plus a renderer version — and an operator can still
//! read what the package actually said, which is the same argument
//! [`crate::ports::ExtractedManifest::license`] already makes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What the stored source *is*, so the reader knows which renderer to run.
///
/// Deliberately not "how it should be displayed": an upstream that declares
/// `text/html` for a document its own protocol says is markdown must not be able
/// to switch which renderer path runs (RFC 0007 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadmeFormat {
    Markdown,
    Html,
    /// reStructuredText. Displayed as escaped source, never rendered: docutils
    /// is the only faithful implementation and a partial one renders some
    /// documents subtly wrong (RFC 0007 §3).
    Rst,
    Plain,
}

impl ReadmeFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Rst => "rst",
            Self::Plain => "plain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "rst" => Some(Self::Rst),
            "plain" => Some(Self::Plain),
            _ => None,
        }
    }
}

/// Where this instance obtained the text.
///
/// Recorded so an operator can tell a README the upstream handed us apart from
/// one read out of the bytes we hold — they can disagree, and which one is on
/// screen is a fact about the answer, not an implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadmeSource {
    /// A field of a metadata document the proxy already fetched — npm's
    /// packument, PyPI's `info.description`, an OpenVSX `files.readme` fetch.
    UpstreamMetadata,
    /// A file inside the artifact, read on the single introspection pass.
    Archive,
    /// The publish request's own metadata, on a local or hybrid registry.
    LocalPublish,
}

impl ReadmeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UpstreamMetadata => "upstream-metadata",
            Self::Archive => "archive",
            Self::LocalPublish => "local-publish",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "upstream-metadata" => Some(Self::UpstreamMetadata),
            "archive" => Some(Self::Archive),
            "local-publish" => Some(Self::LocalPublish),
            _ => None,
        }
    }
}

/// One stored README, for one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageReadme {
    pub registry: String,
    pub name: String,
    pub version: String,
    /// The source text exactly as it arrived, truncated at `max_bytes` and no
    /// more. Never HTML produced by this server.
    pub content: String,
    pub format: ReadmeFormat,
    pub source: ReadmeSource,
    /// Hex SHA-256 of `content` — the render-cache key, and the change detector
    /// that decides whether a re-resolve actually replaced anything.
    pub digest: String,
    /// The source hit `max_bytes` and what is stored is a prefix. Surfaced, so
    /// a reader is never quietly shown half a document as if it were whole.
    pub truncated: bool,
    /// The text is the *package's*, not this version's.
    ///
    /// npm's packument carries a `readme` at the document root as well as per
    /// version. It is attributed to the version `dist-tags.latest` names and
    /// never invented for another one, and the panel says which it is looking at
    /// — presenting a package-level document as a per-version fact is a guess
    /// dressed as an answer (RFC 0007, decision 6).
    pub package_level: bool,
    /// When *this instance* read the text, not when upstream published it.
    pub extracted_at: DateTime<Utc>,
}

/// What a registry client found in the metadata document it already parsed.
///
/// Travels on [`crate::entities::PackageMetadata::extra`] under the `readme`
/// key, which is the existing channel for registry-specific fields — so the four
/// discards of RFC 0007 §2.1 stop being discards without widening
/// `RegistryClient`'s signature.
///
/// Exactly one of `content` and `url` is meaningful: npm and PyPI carry the text,
/// OpenVSX and the VS Code Marketplace carry a link, and the link is followed in
/// a detached task rather than on the resolve path (§7.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataReadme {
    /// The text itself, when the document carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// A URL to read it from, when the document carries only a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What the *protocol* says the markup is — never the upstream's
    /// `Content-Type`, which an upstream could use to switch renderer paths.
    pub format: ReadmeFormat,
    /// The text is the package's, not this version's. See
    /// [`PackageReadme::package_level`].
    #[serde(default)]
    pub package_level: bool,
}

impl MetadataReadme {
    /// The text a document carried inline.
    pub fn text(content: impl Into<String>, format: ReadmeFormat) -> Self {
        Self {
            content: Some(content.into()),
            url: None,
            format,
            package_level: false,
        }
    }

    /// A link a document carried instead of the text.
    pub fn linked(url: impl Into<String>, format: ReadmeFormat) -> Self {
        Self {
            content: None,
            url: Some(url.into()),
            format,
            package_level: false,
        }
    }

    /// Mark this as the package's own README rather than this version's — npm's
    /// document-root `readme`, attributed to the version `dist-tags.latest`
    /// names and to no other.
    pub fn package_level(mut self) -> Self {
        self.package_level = true;
        self
    }

    /// Read the `readme` key out of a `PackageMetadata::extra`.
    ///
    /// A malformed value is `None` rather than an error: `extra` is
    /// registry-specific and best-effort by construction, and a client that
    /// wrote something unexpected should cost a missing README, not a failed
    /// package resolve on a package manager's request path.
    pub fn from_extra(extra: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(extra.get("readme")?.clone()).ok()
    }
}

/// Hex SHA-256 of a README body, the render-cache key and change detector.
pub fn readme_digest(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Whether a version has a README, in the three states the answer really has.
///
/// A boolean cannot carry the third one, and the third one is the common case
/// the moment the page starts answering for packages this instance holds nothing
/// of: `false` rendered for *"we have not looked"* is a definite-looking answer
/// with nothing behind it (RFC 0007 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReadmeState {
    /// There is one, and the endpoint will return it.
    Available,
    /// There genuinely is none — the registry type has no README to give, or
    /// this version's metadata carries an empty one.
    None,
    /// Not determined: an archive-borne type on a version we hold no bytes for,
    /// a `from_archive = false` registry, or a discovery read that was skipped
    /// or failed.
    Unknown,
}

impl ReadmeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

/// What to report for a version with no stored README, given the registry kind.
///
/// Three answers, and the difference matters to a reader:
///
/// - a kind with no README at all is [`ReadmeState::None`] — there is nothing to
///   wait for, and the panel says so as a statement;
/// - a kind that reads its README out of the *archive* is
///   [`ReadmeState::Unknown`] — the text exists inside bytes this instance may
///   not hold, and the panel says it arrives when the version is downloaded;
/// - a metadata-borne kind with nothing stored is also `Unknown` rather than
///   `None`, because the document may simply not have been fetched yet.
///
/// An unknown kind (a registry whose type this binary does not recognise) is
/// `Unknown` for the same reason: not looked is not the same as not there.
///
/// Kept here, beside the state it returns, so the detail handler and the CLI
/// cannot disagree about what an empty result means.
pub fn absent_readme_state_for(kind: Option<crate::entities::RegistryKind>) -> ReadmeState {
    match kind.map(|k| k.readme_support()) {
        Some(crate::entities::ReadmeSupport::None(_)) => ReadmeState::None,
        _ => ReadmeState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_round_trips_every_variant() {
        for f in [
            ReadmeFormat::Markdown,
            ReadmeFormat::Html,
            ReadmeFormat::Rst,
            ReadmeFormat::Plain,
        ] {
            assert_eq!(ReadmeFormat::parse(f.as_str()), Some(f));
        }
        assert_eq!(ReadmeFormat::parse("asciidoc"), None);
    }

    #[test]
    fn source_round_trips_every_variant() {
        for s in [
            ReadmeSource::UpstreamMetadata,
            ReadmeSource::Archive,
            ReadmeSource::LocalPublish,
        ] {
            assert_eq!(ReadmeSource::parse(s.as_str()), Some(s));
        }
        assert_eq!(ReadmeSource::parse("guessed"), None);
    }

    #[test]
    fn digest_is_stable_and_content_addressed() {
        // Two versions with an identical README render once, which is only true
        // if the digest depends on nothing but the bytes.
        assert_eq!(readme_digest("# hi"), readme_digest("# hi"));
        assert_ne!(readme_digest("# hi"), readme_digest("# ho"));
        assert_eq!(readme_digest("").len(), 64);
    }

    /// A kind with no README says `none`; everything else says `unknown`,
    /// because "we have not looked" and "there is none" are different answers
    /// and only one of them is a fact about the package.
    #[test]
    fn an_absent_readme_is_none_only_when_the_kind_has_none_to_give() {
        use crate::entities::RegistryKind;
        assert_eq!(
            absent_readme_state_for(Some(RegistryKind::Maven)),
            ReadmeState::None
        );
        assert_eq!(
            absent_readme_state_for(Some(RegistryKind::Github)),
            ReadmeState::None
        );
        assert_eq!(
            absent_readme_state_for(Some(RegistryKind::Cargo)),
            ReadmeState::Unknown
        );
        assert_eq!(
            absent_readme_state_for(Some(RegistryKind::Npm)),
            ReadmeState::Unknown
        );
        // A registry whose type this binary does not recognise has not been
        // looked at either.
        assert_eq!(absent_readme_state_for(None), ReadmeState::Unknown);
    }

    #[test]
    fn serde_uses_the_wire_strings_the_api_documents() {
        assert_eq!(
            serde_json::to_string(&ReadmeSource::UpstreamMetadata).unwrap(),
            "\"upstream-metadata\""
        );
        assert_eq!(
            serde_json::to_string(&ReadmeState::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&ReadmeFormat::Markdown).unwrap(),
            "\"markdown\""
        );
    }
}
