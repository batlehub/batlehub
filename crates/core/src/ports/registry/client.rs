use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::entities::{PackageId, PackageMetadata};
use crate::error::CoreError;

pub type ArtifactStream = Pin<Box<dyn Stream<Item = Result<Bytes, CoreError>> + Send + 'static>>;

/// The result of fetching an artifact from an upstream registry, including the
/// byte stream and any `Cache-Control` header the upstream returned.
pub struct FetchedArtifact {
    pub stream: ArtifactStream,
    /// Raw `Cache-Control` header value from the upstream artifact response, if any.
    pub cache_control: Option<String>,
}

/// A lightweight package hit returned by upstream search.
#[derive(Debug, Clone)]
pub struct UpstreamPackage {
    pub name: String,
    pub latest_version: String,
    pub description: Option<String>,
}

/// The payload of a version-listing document, in the encoding its protocol uses.
///
/// npm, NuGet and Go's `@latest` are JSON; `maven-metadata.xml` is XML, PyPI's
/// simple index is HTML, Go's `@v/list` is newline-delimited text and cargo's
/// sparse index is NDJSON. Those four travel as [`Self::Text`] with an honest
/// `content_type` on the enclosing [`VersionDocument`].
///
/// There is deliberately **no bytes variant**. Nothing in the supported set is
/// binary, and adding one would invite a signed deb `Packages` index or a
/// RubyGems Marshal blob through a path that must not rewrite either: editing a
/// signed index invalidates its signature and the client rejects the whole
/// repository, which is a worse failure than the one this path exists to fix.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "encoding", content = "content", rename_all = "lowercase")]
pub enum DocumentBody {
    Json(serde_json::Value),
    Text(String),
}

impl DocumentBody {
    /// The JSON value, when this body is JSON. Filters that only understand one
    /// encoding use this to bail out rather than guess.
    pub fn as_json_mut(&mut self) -> Option<&mut serde_json::Value> {
        match self {
            Self::Json(v) => Some(v),
            Self::Text(_) => None,
        }
    }

    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(v) => Some(v),
            Self::Text(_) => None,
        }
    }

    pub fn as_text_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Text(s) => Some(s),
            Self::Json(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Json(_) => None,
        }
    }
}

/// A registry's own version-listing document, as received from upstream.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VersionDocument {
    /// What to send back in `Content-Type`. `application/json` for most,
    /// `text/xml` for `maven-metadata.xml`, `text/html` for a PyPI simple page.
    /// Getting this right is half the reason the listing routes moved off
    /// `proxy_stream`, which served packuments as `application/octet-stream`.
    pub content_type: String,
    pub body: DocumentBody,
    /// `Some(n)` when this document was not received from upstream but
    /// composed from the `n` versions this instance holds (RFC 0008-bis
    /// §4.2). A flag rather than a second type, so every filter and rewrite
    /// a held document goes through applies to a synthesised one unchanged;
    /// the response builder turns it into `X-BatleHub-Listing: synthesised`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesised: Option<u32>,
}

impl VersionDocument {
    /// A JSON document with the usual content type.
    pub fn json(value: serde_json::Value) -> Self {
        Self {
            content_type: "application/json".to_owned(),
            body: DocumentBody::Json(value),
            synthesised: None,
        }
    }

    /// A text document — XML, HTML, NDJSON — with an explicit content type.
    pub fn text(content_type: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            content_type: content_type.into(),
            body: DocumentBody::Text(text.into()),
            synthesised: None,
        }
    }
}

/// Which of a registry's listing documents is being asked for.
///
/// A single method per package name cannot address the registries that have
/// more than one: NuGet serves a flat index *and* registration pages for the
/// same package, RubyGems a versions list *and* a single-gem document,
/// Terraform module versions *and* provider versions. The kind is part of the
/// request, and part of the metadata cache key — without it those documents
/// collide in the cache and one is served under the other's URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentKind {
    /// The registry's primary version listing.
    Versions,
    /// A second listing with a different shape. The string is a stable
    /// discriminant: it names the document in the cache key and in logs, so
    /// changing one is a cache invalidation rather than a rename.
    Secondary(&'static str),
}

impl DocumentKind {
    /// NuGet's registration page, as against its flat index.
    pub const REGISTRATION: Self = Self::Secondary("registration");
    /// RubyGems' single-gem document, as against its versions list.
    pub const GEM: Self = Self::Secondary("gem");
    /// Go's `@latest`, as against `@v/list`.
    pub const LATEST: Self = Self::Secondary("latest");
    /// conda's `current_repodata.json` — the newest-versions-only subset —
    /// as against the full `repodata.json`.
    pub const CURRENT_REPODATA: Self = Self::Secondary("current-repodata");
    /// Composer's `~dev` p2 variant, as against the tagged-release one. A
    /// different document for the same package, not a different encoding.
    pub const P2_DEV: Self = Self::Secondary("p2-dev");
    /// PyPI's PEP 691 JSON simple page, as against the PEP 503 HTML one.
    ///
    /// A separate kind rather than a content negotiation inside one, because
    /// the two are different bytes for the same URL: keyed together in the
    /// metadata cache, whichever representation warmed the entry would be
    /// served to clients that asked for the other.
    pub const SIMPLE_JSON: Self = Self::Secondary("simple-json");
    /// RubyGems' compact index `/versions` — every gem in the registry, one
    /// line each, as against the per-gem JSON APIs.
    ///
    /// RFC 0009 §7.3. Bundler resolves from the compact index first and only
    /// falls back to `specs.4.8.gz` — the one index `listing_filter()` marks
    /// `Unsupported` — when it is absent. Which it was, so every
    /// `bundle install` read the unfiltered index.
    pub const COMPACT_VERSIONS: Self = Self::Secondary("compact-versions");
    /// RubyGems' compact index `/info/{gem}` — one gem's versions and
    /// dependencies, as plain text.
    pub const COMPACT_INFO: Self = Self::Secondary("compact-info");
    /// conda's `channeldata.json` — the cross-platform channel summary
    /// `conda search` reads, as against the per-subdir `repodata.json`.
    pub const CHANNELDATA: Self = Self::Secondary("channeldata");
    /// RubyGems' compact index `/names` — gem names only.
    ///
    /// Names no version, so unlike its two siblings it carries no filtering
    /// obligation: a gem with one blocked version still exists.
    pub const COMPACT_NAMES: Self = Self::Secondary("compact-names");
    /// Terraform's provider *download* document — one platform of one version,
    /// as against the versions listing.
    ///
    /// RFC 0009 §12.12. These are different documents with different shapes,
    /// and the proxy path used to answer a download request with the listing:
    /// no `os`, no `arch`, no `filename`, no `shasum`, and `signing_keys`
    /// defaulted to empty. Terraform 1.8.5 rejected it with *"registry response
    /// to request for linux_amd64 archive has incorrect target _"* — the empty
    /// `os` and `arch` joined by an underscore — so no provider could be
    /// installed through a proxy-mode registry at all.
    ///
    /// The package name carries `{version}/download/{os}/{arch}` because that is
    /// what addresses the document, and it keeps one cache entry per platform
    /// rather than one per provider.
    pub const PROVIDER_DOWNLOAD: Self = Self::Secondary("provider-download");
    /// The `nodejs.org/dist` tree's `index.json` — the same release table as
    /// `index.tab`, as a JSON array — as against the TSV nvm reads.
    ///
    /// RFC 0010 §4.4. Two encodings of one document: nvm resolves every
    /// install through `index.tab`, fnm and mise read `index.json`. A separate
    /// kind because they are different bytes for different URLs, and because
    /// filtering one and not the other would leave an unfiltered answer to the
    /// same question.
    pub const INDEX_JSON: Self = Self::Secondary("index-json");
    /// SDKMAN's `candidates/default/{c}` — the one identifier `sdk install
    /// <candidate>` resolves to when no version is given — as against the
    /// candidate's `versions/all`.
    ///
    /// RFC 0010 §6.2. Names one version and carries no list, so, like Go's
    /// `@latest`, it is repaired against the filtered `versions/all` in the
    /// handler that has both rather than inside `strip`.
    pub const SDKMAN_DEFAULT: Self = Self::Secondary("sdkman-default");
    /// SDKMAN's rendered `candidates/{c}/{plat}/versions/list` — the table
    /// `sdk list <candidate>` prints — as against the comma-separated
    /// `versions/all`.
    ///
    /// The client's `?current=&installed=` query travels in the package
    /// string and is therefore part of the cache key: two clients with
    /// different installed sets must not share an entry (RFC 0010 §6.4).
    pub const SDKMAN_VERSIONS_LIST: Self = Self::Secondary("versions-list");
    /// A protocol document relayed byte-exact, addressed by the upstream path
    /// carried in `package`.
    ///
    /// SDKMAN's `candidates/all`, `candidates/list`, `hooks/{pre,post}/…`,
    /// `healthcheck`, `broker/version/…` and `selfupdate/…` are all text the
    /// client reads as-is, and two of them (the hooks) are bash it sources
    /// and runs — so none is filtered, rewritten or parsed (RFC 0010 §7). One
    /// kind rather than six, because the only thing that varies is the path;
    /// each is still its own cache entry, keyed by that path.
    pub const RELAYED: Self = Self::Secondary("relayed");

    /// The cache-key and log discriminant.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Versions => "versions",
            Self::Secondary(s) => s,
        }
    }
}

impl std::fmt::Display for DocumentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A client for a specific upstream package registry.
///
/// Each registry type (GitHub, Cargo, npm, …) provides its own implementation.
/// Rule evaluation happens in `crates/core/src/rules/` using data returned by this trait.
#[async_trait]
pub trait RegistryClient: Send + Sync {
    /// Short identifier matching the `registry` field of `PackageId` (e.g. `"github"`).
    fn registry_type(&self) -> &str;

    /// Fetch metadata for a package from the upstream registry.
    ///
    /// Implementations should populate `PackageMetadata::published_at` and
    /// `PackageMetadata::is_signed` when the upstream provides that information,
    /// as the rule engine depends on them.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError>;

    /// Stream the raw artifact bytes from the upstream registry, along with any
    /// upstream `Cache-Control` header.
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError>;

    /// Ask upstream whether one artifact is still there, without fetching it
    /// (RFC 0014 §13.5) — a `HEAD` on the file, for the kinds addressed purely
    /// by path, whose [`Self::resolve_metadata`] answers without asking
    /// upstream and therefore cannot tell the audit sweep a file has gone.
    ///
    /// `Ok(())` when upstream confirms it, [`CoreError::NotFound`] when
    /// upstream denies it, any other error when upstream could not answer.
    /// The default is [`CoreError::NotSupported`]: a kind with a metadata
    /// API is probed through that instead, and a capability gap must read
    /// as inconclusive, never as absence.
    async fn probe_artifact(&self, pkg: &PackageId) -> Result<(), CoreError> {
        Err(CoreError::NotSupported(format!(
            "{} has no artifact probe: {pkg} is asked about through its metadata",
            self.registry_type()
        )))
    }

    /// Return all known version strings for `package`, oldest-first.
    ///
    /// The default implementation returns an empty list; registries that do not
    /// support version enumeration (e.g. GitHub Releases, OpenVSX) can rely on it.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let _ = package;
        Ok(vec![])
    }

    /// Fetch one of the upstream's own *version-listing documents* for
    /// `package`, as received — a document a client resolves a version range
    /// against.
    ///
    /// Distinct from [`Self::list_versions`], which answers "which versions
    /// exist" for internal use (cache warming). This returns the document
    /// itself, because the proxy has to hand the client something in the shape
    /// its package manager expects: for npm that is the packument, with its
    /// `versions` map, `dist-tags` and per-version metadata.
    ///
    /// The proxy needs the parsed document — rather than streaming the upstream
    /// response through — so it can remove administratively blocked versions
    /// before a resolver ever sees them (see
    /// [`crate::services::blocking`]). A streaming rewrite could not do it:
    /// recomputing `dist-tags.latest` or a registration page's `count` needs the
    /// whole document.
    ///
    /// `kind` selects between the listings of a registry that has more than one
    /// (see [`DocumentKind`]); implementations that have exactly one should
    /// serve [`DocumentKind::Versions`] and reject the rest with
    /// [`CoreError::NotSupported`] rather than quietly returning the wrong
    /// document.
    ///
    /// The default returns [`CoreError::NotSupported`]; only registry types
    /// whose protocol has such a document implement it.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let _ = (package, kind);
        Err(CoreError::NotSupported(
            "this registry type has no version-listing document".to_owned(),
        ))
    }

    /// Read a README the metadata *linked to* rather than carried.
    ///
    /// OpenVSX and the VS Code Marketplace give a URL, not text, so reading it
    /// is an outbound request in its own right. It is never made on the resolve
    /// path — the link travels on `PackageMetadata::extra` and is followed in
    /// the detached introspection task (RFC 0007 §5.1).
    ///
    /// The implementation is responsible for the guards, because it is the only
    /// layer that knows this registry's own base URL: the same
    /// `ensure_same_origin` check tarball URLs already get, so a compromised or
    /// misconfigured upstream cannot use this to point BatleHub at an internal
    /// host; the shared `UpstreamHttpOptions` client with its timeouts and SSRF
    /// guards; and a body read incrementally to `max_bytes` rather than
    /// buffered-then-truncated.
    ///
    /// The response's `Content-Type` must **not** decide the format: the
    /// protocol's declared one does, so an upstream cannot switch which renderer
    /// path runs (§7.4).
    ///
    /// The default returns [`CoreError::NotSupported`]; only the two linked
    /// kinds implement it.
    async fn fetch_linked_readme(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Option<String>, CoreError> {
        let _ = (url, max_bytes);
        Err(CoreError::NotSupported(
            "this registry type does not link to a README".to_owned(),
        ))
    }

    /// The forge-specific questions this client answers, when it is a forge
    /// (RFC 0019 §6.1): ref resolution and commit lookup.
    ///
    /// `None` for every package registry, and nothing else changes for them.
    /// A forge client returns `Some(self)`, which is what lets `ProxyService`
    /// resolve a ref to a commit before it fetches, without the proxy knowing
    /// which forge it is talking to.
    fn forge(&self) -> Option<&dyn super::super::forge::ForgeRegistry> {
        None
    }

    /// This client's releases, normalised across forges (RFC 0021 §5.2).
    ///
    /// `None` for every package registry, and `Some(self)` for the three that
    /// serve releases — the same arrangement [`Self::forge`] uses, and for the
    /// same reason: an import chooses a release without knowing which forge
    /// answered, and `AppConfig::validate()` has already refused a `from` that
    /// is not one of the three.
    fn releases(&self) -> Option<&dyn super::ForgeReleaseSource> {
        None
    }

    /// Search the upstream registry for packages matching `query`.
    ///
    /// Returns up to `limit` results. The default implementation returns an empty
    /// list; registries without a search API (e.g. GitHub, Go) can rely on it.
    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        let _ = (query, limit);
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct MinimalClient;

    #[async_trait]
    impl RegistryClient for MinimalClient {
        fn registry_type(&self) -> &str {
            "minimal"
        }
        async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            Ok(PackageMetadata {
                id: pkg.clone(),
                published_at: None,
                download_url: None,
                checksum: None,
                is_signed: None,
                extra: serde_json::Value::Null,
                cache_control: None,
            })
        }
        async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
            Err(CoreError::NotFound("no artifact".into()))
        }
        // Does NOT override list_versions or search_packages → uses defaults
    }

    #[tokio::test]
    async fn default_list_versions_returns_empty() {
        let client = MinimalClient;
        let versions = client.list_versions("some-pkg").await.unwrap();
        assert!(versions.is_empty());
    }

    #[tokio::test]
    async fn default_search_packages_returns_empty() {
        let client = MinimalClient;
        let results = client.search_packages("query", 10).await.unwrap();
        assert!(results.is_empty());
    }
}
