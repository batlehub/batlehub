use serde::{Deserialize, Serialize};

/// Whether blocked versions are removed from one listing document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingSupport {
    /// Blocked versions are absent from this document, and whatever it calls
    /// "newest" is repaired to name a version that is still allowed.
    Filtered,
    /// Filtered, with a caveat an operator has to know about. The string
    /// completes "yes — …".
    Qualified(&'static str),
    /// Not filtered, and why. The string completes "no — …".
    ///
    /// The reason travels with the code that decides it so the published
    /// coverage table cannot drift from the behaviour. Two reasons exist today:
    /// editing a signed repository index invalidates its signature and the
    /// client rejects the whole repository (a worse failure than the one
    /// filtering fixes), and RubyGems' Marshal indexes would need a Ruby
    /// Marshal encoder in Rust to hide a version its JSON APIs already hide for
    /// every client released this decade.
    Unsupported(&'static str),
}

/// One version-listing document a registry kind serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingDocument {
    /// How the admin guide names it — "packument", "`maven-metadata.xml`".
    pub label: &'static str,
    pub support: ListingSupport,
    /// The `DocumentKind` discriminants this row covers — the same strings
    /// `DocumentKind::as_str()` returns and the metadata cache key uses.
    ///
    /// Strings rather than the enum because `entities` does not depend on
    /// `ports`, and this is the one fact it needs from there. They are checked
    /// against the real enum in `blocking`'s
    /// `every_advertised_filter_is_reachable_from_dispatch`, which resolves each
    /// one and fails on a name no `DocumentKind` answers to — so a typo here is
    /// a test failure, not a silently skipped document.
    ///
    /// One row may cover several: PyPI's HTML and PEP 691 JSON pages are one
    /// *document* to a reader of the admin guide and two `DocumentKind`s to the
    /// cache (RFC 0006 §13.4). Empty means the row describes something that
    /// never travels through `strip` at all — a signed index, or a kind that
    /// filters at a handler chokepoint.
    ///
    /// RFC 0009 §4.1: the reachability contract used to be exhaustive over
    /// *kinds* and blind to *documents*, so a kind already answering "filtered"
    /// for one document could grow a second, unfiltered one in silence. This
    /// field is what closes that.
    pub documents: &'static [&'static str],
}

impl ListingDocument {
    const fn filtered(label: &'static str, documents: &'static [&'static str]) -> Self {
        Self {
            label,
            support: ListingSupport::Filtered,
            documents,
        }
    }
    const fn qualified(
        label: &'static str,
        note: &'static str,
        documents: &'static [&'static str],
    ) -> Self {
        Self {
            label,
            support: ListingSupport::Qualified(note),
            documents,
        }
    }
    /// No `documents`: an unsupported row names something `strip` never sees.
    const fn unsupported(label: &'static str, reason: &'static str) -> Self {
        Self {
            label,
            support: ListingSupport::Unsupported(reason),
            documents: &[],
        }
    }
}

/// Where a registry kind's README comes from, if it has one at all.
///
/// A property of the *protocol*, not a preference: whether the text arrives in a
/// document the proxy already fetches to resolve a version, or only inside the
/// artifact, is decided by the ecosystem and decides in turn whether a version
/// this instance holds no bytes for can have one (RFC 0007 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadmeSupport {
    /// The text is a field of a metadata document the proxy already parses.
    /// Available for every version upstream knows about.
    Metadata,
    /// The metadata carries a *URL*, not the text. Reading it is an outbound
    /// request in its own right, same-origin checked against the registry's own
    /// base URL (RFC 0007 §7.4).
    MetadataLinked,
    /// The text is a file inside the artifact. Available only for versions this
    /// instance has cached or hosts — the same honest limit `license` has.
    Archive,
    /// Metadata when the document carries it, the artifact when it does not.
    /// npm is the only one: the packument's `readme` is often empty for versions
    /// published by tooling that only writes the tarball.
    MetadataThenArchive,
    /// This kind has no README to give, and why.
    ///
    /// The reason travels with the code that decides it so the published support
    /// table cannot drift from the behaviour, exactly as [`ListingSupport`] does.
    /// The string completes "no — …".
    None(&'static str),
}

impl ReadmeSupport {
    /// Whether any README can ever be stored for this kind.
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::None(_))
    }

    /// Whether the artifact has to be opened for this kind's README, i.e.
    /// whether `from_archive` means anything on a registry of this kind.
    pub fn reads_the_archive(&self) -> bool {
        matches!(self, Self::Archive | Self::MetadataThenArchive)
    }

    /// Whether a version this instance holds no bytes for can have a README —
    /// the *unheld* column of the support table.
    pub fn answers_for_unheld_versions(&self) -> bool {
        matches!(
            self,
            Self::Metadata | Self::MetadataLinked | Self::MetadataThenArchive
        )
    }
}

/// How this kind answers "what versions exist upstream" for the console's
/// discovery read (RFC 0007 §4.3, §5.5).
///
/// The *unheld* column of the support table. Which arm a kind takes decides how
/// much the package page can say about a package this instance holds nothing of:
/// a listing document carries publish times and, for npm, the README; a bare
/// version list carries neither, which is honest and still the difference
/// between a versions table and an empty state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamDetailSupport {
    /// Read this kind's listing document, and name *which* one: the kinds with
    /// more than one would otherwise have the choice made twice, once here and
    /// once at the fetch.
    ///
    /// The string is the same discriminant `DocumentKind::as_str` returns, for
    /// the reason [`ListingDocument::documents`] carries strings: `entities`
    /// does not depend on `ports`, and this is the one fact it needs from there.
    /// Checked against the real enum by the drift test in
    /// `services::upstream_detail`, so a typo is a test failure rather than a
    /// silently skipped kind.
    Document(&'static str),
    /// This kind has no listing document the proxy can read, but it can
    /// enumerate versions. Produces rows with no publish times — honest, and
    /// enough for a table.
    ListVersions,
    /// There is nothing to ask, and why. The string completes "no — …".
    None(&'static str),
}

impl UpstreamDetailSupport {
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::None(_))
    }
}

/// The protocol a registry adapter speaks — e.g. `"cargo"`, `"npm"`, `"maven"`.
///
/// Distinct from a registry's user-configured *instance* name (e.g. `"my-maven"`
/// in `RegistryConfig.name`, or `RegistryMap`'s keys): many instances of the same
/// type can be configured, each proxying a different upstream.
///
/// Serializes/deserializes as the same kebab-case strings the TOML config and
/// wire format already use, so this is a drop-in replacement for the bare
/// `String` — the one place those strings must stay in sync (this enum) is now
/// compiler-enforced instead of hand-synced across `crates/config`'s validator
/// and `server/src/builders.rs`'s client-construction match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryKind {
    Github,
    Forgejo,
    Gitlab,
    Cargo,
    Npm,
    Openvsx,
    Goproxy,
    Pypi,
    Conda,
    Composer,
    VscodeMarketplace,
    Maven,
    Terraform,
    Rubygems,
    Nuget,
    Deb,
    Rpm,
    Pacman,
    Jetbrains,
    JetbrainsMarketplace,
    Generic,
    /// The `nodejs.org/dist` file tree as a *typed* registry — one package
    /// (`node`), one version per release, one file per platform — so a Node
    /// release can be blocked rather than merely cached (RFC 0010).
    Nodedist,
    /// SDKMAN's candidates API and download broker as one registry: the JDK,
    /// Gradle, Maven-the-distribution, Kotlin — addressed by
    /// `{candidate}/{version}/{platform}`, the broker's `302` to a third-party
    /// CDN followed server-side through the SSRF guard (RFC 0010).
    Sdkman,
}

impl RegistryKind {
    /// All known registry kinds, in the same order the config validator and
    /// `server/src/builders.rs` have historically listed them.
    pub const ALL: &[RegistryKind] = &[
        Self::Github,
        Self::Forgejo,
        Self::Gitlab,
        Self::Cargo,
        Self::Npm,
        Self::Openvsx,
        Self::Goproxy,
        Self::Pypi,
        Self::Conda,
        Self::Composer,
        Self::VscodeMarketplace,
        Self::Maven,
        Self::Terraform,
        Self::Rubygems,
        Self::Nuget,
        Self::Deb,
        Self::Rpm,
        Self::Pacman,
        Self::Jetbrains,
        Self::JetbrainsMarketplace,
        Self::Generic,
        Self::Nodedist,
        Self::Sdkman,
    ];

    /// The kebab-case wire string for this kind (matches TOML `type = "..."`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Forgejo => "forgejo",
            Self::Gitlab => "gitlab",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Openvsx => "openvsx",
            Self::Goproxy => "goproxy",
            Self::Pypi => "pypi",
            Self::Conda => "conda",
            Self::Composer => "composer",
            Self::VscodeMarketplace => "vscode-marketplace",
            Self::Maven => "maven",
            Self::Terraform => "terraform",
            Self::Rubygems => "rubygems",
            Self::Nuget => "nuget",
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Pacman => "pacman",
            Self::Jetbrains => "jetbrains",
            Self::JetbrainsMarketplace => "jetbrains-marketplace",
            Self::Generic => "generic",
            Self::Nodedist => "nodedist",
            Self::Sdkman => "sdkman",
        }
    }

    /// The name a blocked-version lookup should use for `package`.
    ///
    /// Defaults to the name itself. `sdkman` addresses its listing documents
    /// by `{candidate}/{platform}` because `fetch_version_document` has
    /// nowhere else to put the platform, but a block is a statement about the
    /// candidate: an admin blocking a JDK means all eight platforms, not the
    /// one whose listing they happened to be looking at (RFC 0010 §6.2,
    /// decision 8). Every other kind returns `package` unchanged, so the
    /// call sites in `ProxyService::version_document` are inert for them.
    pub fn blocking_package_name<'a>(&self, package: &'a str) -> &'a str {
        match self {
            Self::Sdkman => crate::services::sdkman::candidate_of(package),
            _ => package,
        }
    }

    /// Local/hybrid mode is only meaningful for registries this proxy can host
    /// package versions for itself — the read-only source-hosting types
    /// (github/forgejo/gitlab/jetbrains) have no local publish model. `generic`
    /// is proxy-only for now; hosting arbitrary files is a separate roadmap item.
    /// `nodedist` and `sdkman` have no publish protocol either: Node releases
    /// are built by the Node project and SDKMAN's candidates by their vendors,
    /// and hosting a private toolchain is a separate feature (RFC 0010 §3).
    pub fn supports_local_mode(&self) -> bool {
        !matches!(
            self,
            Self::Github
                | Self::Forgejo
                | Self::Gitlab
                | Self::Jetbrains
                | Self::Generic
                | Self::Nodedist
                | Self::Sdkman
        )
    }

    /// `deb`/`rpm` have no universal default upstream (unlike e.g. `npm`'s
    /// registry.npmjs.org), so proxy mode requires an explicit upstream —
    /// otherwise every fetch would silently hit an unreachable placeholder.
    /// `generic` mirrors an arbitrary file tree, so it has no default at all.
    pub fn requires_explicit_upstream_in_proxy_mode(&self) -> bool {
        matches!(self, Self::Deb | Self::Rpm | Self::Generic)
    }

    /// Whether this kind is a git forge — GitHub, GitLab, Forgejo — and so
    /// speaks in refs rather than versions (RFC 0019). The kinds
    /// `[registries.refs]` means something on.
    pub fn is_forge(&self) -> bool {
        matches!(self, Self::Github | Self::Gitlab | Self::Forgejo)
    }

    /// Whether this kind is addressed purely by upstream file path, with the
    /// whole path carried in `PackageId::artifact` and no per-package metadata
    /// API — the kinds served by `PathProxyRegistryClient`. These are the kinds
    /// for which `path_allow` and `cache.warm_paths` are meaningful.
    pub fn is_path_addressed(&self) -> bool {
        matches!(
            self,
            Self::Deb | Self::Rpm | Self::Pacman | Self::Jetbrains | Self::Generic
        )
    }

    /// The version-listing documents this kind serves, and whether blocked
    /// versions are hidden from each.
    ///
    /// The single source of truth for "which registries filter their listings".
    /// The coverage table in `docs/guide/admin-policies.md` is *generated* from
    /// this rather than maintained beside it, because the previous arrangement —
    /// a warning box in the admin guide naming npm as the only filtered
    /// ecosystem — was a fact about the code that nothing kept true.
    ///
    /// Exhaustive with no wildcard arm, so adding a registry kind does not
    /// compile until it answers the question, in the same way
    /// `server/src/builders.rs`'s match already forces a decision about client
    /// construction. An empty slice is a legitimate answer and means the
    /// protocol has no version listing at all.
    ///
    /// `blocking::strip` is checked against this in
    /// `every_advertised_filter_is_reachable_from_dispatch`: a document
    /// advertised here as filtered must reach a real filter.
    pub fn listing_filter(&self) -> &'static [ListingDocument] {
        // Named consts rather than inline slice literals: a `&[...]` holding
        // const-fn calls is not promoted to `'static`, so each arm needs a
        // const item to borrow from.
        const NPM: &[ListingDocument] = &[ListingDocument::filtered("packument", &["versions"])];
        const NUGET: &[ListingDocument] = &[
            ListingDocument::filtered("flat index", &["versions"]),
            ListingDocument::qualified(
                "registration pages",
                "inline pages only; paged registrations pass through, and are logged",
                &["registration"],
            ),
        ];
        const MAVEN: &[ListingDocument] = &[ListingDocument::filtered(
            "`maven-metadata.xml`",
            &["versions"],
        )];
        const PYPI: &[ListingDocument] = &[ListingDocument::filtered(
            "simple index (HTML and PEP 691 JSON)",
            &["versions", "simple-json"],
        )];
        const CARGO: &[ListingDocument] = &[ListingDocument::qualified(
            "sparse index",
            "blocked versions are marked `yanked` rather than removed, which is cargo's own \
             mechanism for \"exists, do not select\" and keeps lockfile diagnostics honest",
            &["versions"],
        )];
        const GOPROXY: &[ListingDocument] = &[ListingDocument::filtered(
            "`@v/list` and `@latest`",
            &["versions", "latest"],
        )];
        const RUBYGEMS: &[ListingDocument] = &[
            // `/versions` is whole-registry and filters through `dispatch_multi`,
            // so it carries the same up-to-30-second snapshot lag conda does.
            ListingDocument::qualified(
                "compact index (`/versions`, `/info/{gem}`)",
                "`/versions` describes the whole registry, so a new block reaches it within \
                 the blocked-set snapshot's 30-second TTL rather than instantly; `/info` is \
                 per-gem and immediate",
                &["compact-versions", "compact-info"],
            ),
            ListingDocument::filtered("versions and gem JSON APIs", &["versions", "gem"]),
            ListingDocument::unsupported(
                "`specs.4.8.gz`, `quick/Marshal.4.8`",
                "hiding a version from a Ruby Marshal index would need a Marshal encoder in \
                 Rust, and nothing reads it: Bundler resolves from the compact index above, \
                 and the JSON APIs answer every other client released this decade",
            ),
        ];
        const COMPOSER: &[ListingDocument] = &[ListingDocument::filtered(
            "p2 metadata",
            &["versions", "p2-dev"],
        )];
        const TERRAFORM: &[ListingDocument] = &[ListingDocument::filtered(
            "module and provider versions",
            &["versions"],
        )];
        // Filtered through `dispatch_multi` (a whole-channel blocked set), so
        // the documents are listed for the admin table but `strip` never sees
        // them — the exemption is `FILTERED_ELSEWHERE`.
        const CONDA: &[ListingDocument] = &[
            ListingDocument::filtered(
                "`repodata.json`, `current_repodata.json` (and their `.zst`/`.bz2` encodings)",
                &["versions", "current-repodata"],
            ),
            ListingDocument::qualified(
                "`channeldata.json`",
                "a blocked newest release drops the package from the channel summary rather \
                 than moving it to an older one: channeldata names one version and carries no \
                 list to pick a replacement from, so `conda search` stops showing it while \
                 `conda install` still resolves it from `repodata.json`",
                &["channeldata"],
            ),
        ];
        // Filtered at a handler chokepoint, not through `strip` — three
        // rendered documents from one intermediate version list.
        const JETBRAINS_MARKETPLACE: &[ListingDocument] = &[ListingDocument::filtered(
            "`updatePlugins.xml`, `/plugins/list` and the plugin-updates API",
            &[],
        )];
        const FORGE: &[ListingDocument] =
            &[ListingDocument::filtered("release listings", &["versions"])];
        const SIGNED: &[ListingDocument] = &[ListingDocument::unsupported(
            "signed repository indexes",
            "editing one invalidates its signature and the client rejects the whole \
             repository, which is a worse failure than the one filtering fixes",
        )];
        // Filtered on the entries in `vsx/source.rs`: the gallery response is
        // selected by a POST body rather than a URL, so `strip`'s
        // `(kind, document, package)` signature cannot address it, and the same
        // entries render into two protocols (RFC 0006 §13.1-bis).
        const EXTENSION_GALLERY: &[ListingDocument] = &[ListingDocument::filtered(
            "extension gallery (`extensionquery`) and the OpenVSX API",
            &[],
        )];
        // Two encodings of one document. nvm resolves *every* install through
        // `index.tab`; fnm and mise read `index.json`. Filtering one and not
        // the other would leave a second, unfiltered answer to the same
        // question (RFC 0010 §4.4). `SHASUMS256.txt` is deliberately not a
        // listing: it is signed by a sibling `.asc`/`.sig` and never rewritten.
        const NODEDIST: &[ListingDocument] = &[
            ListingDocument::filtered("`index.tab`", &["versions"]),
            ListingDocument::filtered("`index.json`", &["index-json"]),
        ];
        // Three text documents (RFC 0010 §4.4). `versions/all` and
        // `candidates/default` are what `sdk install` resolves through; the
        // rendered `versions/list` is what `sdk list` prints, filtered in both
        // of its fixed-width layouts so the console never advertises a JDK the
        // install then refuses (decision 5). The chokepoint itself,
        // `candidates/validate`, is not a listing: it answers `invalid` for a
        // blocked version in the handler.
        const SDKMAN: &[ListingDocument] = &[
            ListingDocument::filtered("`versions/all`", &["versions"]),
            ListingDocument::filtered("`candidates/default`", &["sdkman-default"]),
            ListingDocument::filtered(
                "the rendered `versions/list` table (`sdk list`)",
                &["versions-list"],
            ),
        ];

        match self {
            Self::Npm => NPM,
            Self::Nuget => NUGET,
            Self::Maven => MAVEN,
            Self::Pypi => PYPI,
            Self::Cargo => CARGO,
            Self::Goproxy => GOPROXY,
            Self::Rubygems => RUBYGEMS,
            Self::Composer => COMPOSER,
            Self::Terraform => TERRAFORM,
            Self::Conda => CONDA,
            Self::JetbrainsMarketplace => JETBRAINS_MARKETPLACE,
            Self::Github | Self::Gitlab | Self::Forgejo => FORGE,
            Self::Deb | Self::Rpm | Self::Pacman => SIGNED,
            Self::Openvsx | Self::VscodeMarketplace => EXTENSION_GALLERY,
            Self::Nodedist => NODEDIST,
            Self::Sdkman => SDKMAN,
            // `generic` and `jetbrains` mirror an arbitrary file tree by path —
            // there is no listing document in the protocol at all, so there is
            // nothing to say beyond that. (JetBrains *plugins* are the separate
            // `jetbrains-marketplace` kind above, and are filtered; VS Code
            // extensions are `openvsx`/`vscode-marketplace` above.)
            Self::Generic | Self::Jetbrains => &[],
        }
    }

    /// Where this kind's README comes from, or why it has none.
    ///
    /// The single source of truth for the per-type support table in
    /// `docs/registries/`, which is *generated* from this rather than maintained
    /// beside it — the same arrangement [`Self::listing_filter`] has, and for the
    /// same reason: a published table claiming coverage the code does not deliver
    /// is a statement about the product that is not true.
    ///
    /// Exhaustive with no wildcard arm, so a new registry kind does not compile
    /// until it answers the question.
    pub fn readme_support(&self) -> ReadmeSupport {
        match self {
            // The packument carries `readme` per version and at the document
            // root; when a publisher's tooling wrote neither, the tarball still
            // has one.
            Self::Npm => ReadmeSupport::MetadataThenArchive,
            // `info.description` with `info.description_content_type` naming
            // its markup. Per version by construction, and the archive says the
            // same thing from the other side: the JSON API's `description` *is*
            // the wheel's `METADATA` body, so a version resolved through the
            // simple index rather than the JSON API still gets one.
            Self::Pypi => ReadmeSupport::MetadataThenArchive,
            // The plugin `<description>`, already HTML.
            Self::JetbrainsMarketplace => ReadmeSupport::Metadata,
            // `files.readme` and the `Content.Details` asset are URLs.
            Self::Openvsx | Self::VscodeMarketplace => ReadmeSupport::MetadataLinked,
            // The sparse index has no README field; the `.crate` does, named by
            // `Cargo.toml [package] readme`. On publish the metadata carries the
            // full text, which is why cargo has a `LocalPublish` source too.
            Self::Cargo => ReadmeSupport::Archive,
            // The `.nuspec` `<readme>` element names a file inside the `.nupkg`.
            Self::Nuget => ReadmeSupport::Archive,
            // `README*` at the module root of the `.zip`.
            Self::Goproxy => ReadmeSupport::Archive,
            // `README*` at the root of a module tarball. Providers have none,
            // which the extractor answers for by finding no file.
            Self::Terraform => ReadmeSupport::Archive,
            // `README*` at the root of the dist zip.
            Self::Composer => ReadmeSupport::Archive,
            // `info/about.json` → `description`, as plain text.
            Self::Conda => ReadmeSupport::Archive,
            // `README*` in `data.tar.gz`. There is no declared field in a
            // gemspec, so this is a filename convention rather than a protocol
            // guarantee (RFC 0007 open question 3).
            Self::Rubygems => ReadmeSupport::Archive,
            Self::Maven => ReadmeSupport::None(
                "the POM carries `<description>`, which is a sentence rather than a document; \
                 putting one where a reader expects the other makes every package look thinly \
                 documented",
            ),
            Self::Deb | Self::Rpm | Self::Pacman | Self::Jetbrains | Self::Generic => {
                ReadmeSupport::None(
                    "path-addressed: there is no package identity to hang a README on",
                )
            }
            Self::Github | Self::Gitlab | Self::Forgejo => ReadmeSupport::None(
                "the README is one of the repository files this proxy already serves by path, \
                 under `raw/{ref}/`, so a second URL for it would be a second answer to a \
                 solved question",
            ),
            Self::Nodedist => ReadmeSupport::None(
                "a Node release is a set of tarballs and a checksum file; the dist tree carries \
                 no prose",
            ),
            Self::Sdkman => ReadmeSupport::None(
                "SDKMAN describes a distribution, not a package: no document in the protocol \
                 carries prose about a candidate",
            ),
        }
    }

    /// How this kind answers the console's discovery read.
    ///
    /// The other half of the per-type support table, and generated into it for
    /// the same reason [`Self::readme_support`] is. Exhaustive with no wildcard
    /// arm, so a new registry kind does not compile until it answers.
    pub fn upstream_detail(&self) -> UpstreamDetailSupport {
        match self {
            // The packument: versions, `time`, `dist-tags`, and the README.
            // One cached fetch answers both halves of what the page is missing.
            Self::Npm => UpstreamDetailSupport::Document("versions"),
            // The PEP 691 JSON simple page, not the HTML one: same document,
            // and a parser rather than an HTML scrape.
            Self::Pypi => UpstreamDetailSupport::Document("simple-json"),
            // The sparse index, one JSON object per line, carrying `yanked`.
            Self::Cargo => UpstreamDetailSupport::Document("versions"),
            // The flat index — the document that resolves a version.
            Self::Nuget => UpstreamDetailSupport::Document("versions"),
            // `@v/list`, one version per line.
            Self::Goproxy => UpstreamDetailSupport::Document("versions"),
            // `maven-metadata.xml`.
            Self::Maven => UpstreamDetailSupport::Document("versions"),
            // p2 metadata.
            Self::Composer => UpstreamDetailSupport::Document("versions"),
            // The versions API.
            Self::Rubygems => UpstreamDetailSupport::Document("versions"),
            // Module and provider versions. The explore name carries the
            // `modules/`/`providers/` prefix the protocol addresses by.
            Self::Terraform => UpstreamDetailSupport::Document("versions"),
            // `repodata.json` describes a whole channel, not a package: reading
            // one package's versions out of it would fetch and parse every
            // package's on every page view. `list_versions` asks the same
            // question at the size of the answer.
            Self::Conda => UpstreamDetailSupport::ListVersions,
            // The extension galleries answer by query rather than by document —
            // `allVersions` and `extensionquery` — which is what
            // `list_versions` already wraps.
            Self::Openvsx | Self::VscodeMarketplace | Self::JetbrainsMarketplace => {
                UpstreamDetailSupport::ListVersions
            }
            Self::Github | Self::Gitlab | Self::Forgejo => UpstreamDetailSupport::None(
                "a release listing is this instance's own view of a repository it proxies by                  path, and the console's package page is not where a repository is browsed",
            ),
            Self::Deb | Self::Rpm | Self::Pacman | Self::Jetbrains | Self::Generic => {
                UpstreamDetailSupport::None(
                    "path-addressed: there is no package identity to ask about",
                )
            }
            // `index.tab`: one row per release, with its date and LTS codename.
            Self::Nodedist => UpstreamDetailSupport::Document("versions"),
            // `versions/all` for the candidate on the default platform — the
            // identifiers and nothing else; SDKMAN publishes no dates.
            Self::Sdkman => UpstreamDetailSupport::Document("versions"),
        }
    }

    /// Whether "fetch this version" has a single meaning for this kind
    /// (RFC 0007-bis §4.4).
    ///
    /// The button on an upstream-only row runs the ordinary download path for
    /// one coordinate. That needs the coordinate to *name* one thing: Maven's
    /// artifact is a set of files, and a Terraform provider needs an OS and an
    /// architecture, so for those the console renders the reason rather than a
    /// disabled button with no explanation.
    ///
    /// Exhaustive with no wildcard arm, for the same reason
    /// [`Self::readme_support`] and [`Self::upstream_detail`] are: a new kind
    /// does not compile until somebody decides.
    pub fn fetchable_by_version(&self) -> FetchSupport {
        match self {
            // Every one of these addresses its artifact with a sub-coordinate on
            // the *download* path, and the coordinate alone is not the key.
            // This table used to say `ByVersion` with no sub-coordinate for all
            // of them, which made the button write `artifact:{reg}/lodash/4.17.21`
            // while `npm install` reads `artifact:{reg}/lodash/4.17.21/tarball`:
            // the fetch reported success, filled a slot nothing reads, and the
            // next real install still went upstream. The same mismatch made the
            // `409 already-held` check unable to match a genuinely cached
            // version. Exactly the bug [`Self::warm_artifact`] documents for
            // vsix, one table over.
            //
            // Worse than a dead key for two of them: with no sub-coordinate the
            // goproxy client serves `@v/{version}.info` (a JSON stamp, not the
            // module) and the RubyGems client serves `api/v1/gems/{name}.json`
            // (the metadata document, not the gem), so the button downloaded the
            // wrong thing and cached it under the name of the right one.
            Self::Npm => FetchSupport::ByVersion(FetchArtifact::Fixed("tarball")),
            Self::Cargo => FetchSupport::ByVersion(FetchArtifact::Fixed("dl")),
            Self::Composer => FetchSupport::ByVersion(FetchArtifact::Fixed("dist")),
            Self::Rubygems => FetchSupport::ByVersion(FetchArtifact::Fixed("gem")),
            // `.zip` — the module source. `.mod` is the other half of what `go
            // mod download` reads, but it is a few hundred bytes next to the
            // archive, and "fetch this version" means the bytes.
            Self::Goproxy => FetchSupport::ByVersion(FetchArtifact::Fixed("zip")),
            // Addressed by filename, but the filename is derivable — see
            // [`FetchArtifact::NugetFlat`].
            Self::Nuget => FetchSupport::ByVersion(FetchArtifact::NugetFlat),
            // The extension and plugin galleries address their artifact with a
            // sub-coordinate too; these two are the ones that always said so.
            Self::Openvsx | Self::VscodeMarketplace => {
                FetchSupport::ByVersion(FetchArtifact::Fixed("vsix"))
            }
            Self::JetbrainsMarketplace => FetchSupport::ByVersion(FetchArtifact::Fixed("plugin")),
            // A PyPI version is a set of distribution files — an sdist and one
            // wheel per interpreter/platform tag — and the proxy addresses each
            // by its full filename, which is not derivable from the coordinate
            // (the tags are not in it). The same reasoning as Maven below, and
            // the same answer. Said plainly rather than left as a button that
            // could not work: with no filename the PyPI client looks for a file
            // named `""` in the version's `urls` and returns `NotFound`, so this
            // row's button was a `404` generator.
            Self::Pypi => FetchSupport::None(
                "a PyPI version is a set of distribution files — an sdist and one wheel per \
                 interpreter and platform — each addressed by its own filename, so \"fetch this \
                 version\" has no single meaning",
            ),
            // Conda is refused for a second, sharper reason: this proxy carries
            // the *platform* in `PackageId::version` (see the conda client's
            // `resolve_metadata`), and the artifact filename additionally
            // carries the build string. A fetch of `numpy 1.24.0` would issue
            // `GET {base}/1.24.0/repodata.json` — a path that cannot exist.
            Self::Conda => FetchSupport::None(
                "a conda artifact is addressed by channel platform and build string as well as \
                 version, so \"fetch this version\" has no single meaning",
            ),
            Self::Maven => FetchSupport::None(
                "a Maven version is a set of files — a jar, a pom, sources, javadoc — so \
                 \"fetch this version\" has no single meaning",
            ),
            Self::Terraform => FetchSupport::None(
                "a Terraform provider binary is addressed by OS and architecture as well as \
                 version, and a module is fetched by the client from a URL this instance \
                 rewrites rather than as one artifact",
            ),
            Self::Deb | Self::Rpm | Self::Pacman | Self::Jetbrains | Self::Generic => {
                FetchSupport::None("path-addressed: there is no version to fetch by")
            }
            Self::Github | Self::Gitlab | Self::Forgejo => FetchSupport::None(
                "a release asset is addressed by its filename, which the page does not know",
            ),
            // Maven's reasoning, one tree over: the file name is not a
            // constant, so warming cannot name it either (RFC 0010 §6.1).
            Self::Nodedist => FetchSupport::None(
                "a Node release is a set of files — one per platform, plus headers, source and \
                 checksums — so \"fetch this version\" has no single meaning",
            ),
            // Terraform's reasoning: the artifact needs a platform as well as
            // a version, and the platform is not a constant (RFC 0010 §6.1).
            Self::Sdkman => FetchSupport::None(
                "an SDKMAN artifact is addressed by platform as well as version — one archive \
                 per platform — so \"fetch this version\" has no single meaning",
            ),
        }
    }

    /// The `PackageId::artifact` sub-coordinate this kind's primary downloadable
    /// artifact is cached under, when it uses one.
    ///
    /// Proxy cache keys are `artifact:` + [`crate::entities::PackageId::cache_key`], i.e.
    /// `artifact:{registry}/{name}/{version}[/{artifact}]`. Warming reads this
    /// so a pre-fetched artifact lands in the exact slot the proxy read path
    /// looks in, instead of a slot nothing ever reads.
    ///
    /// **Derived from [`Self::fetchable_by_version`], not restated.** Warming
    /// and the console's fetch button ask the same question — *where does this
    /// kind's one downloadable artifact live?* — and answering it twice is how
    /// this went wrong before: the fetch table said "by version, no
    /// sub-coordinate" for eight kinds whose download path uses one. A kind
    /// that cannot name a single artifact for a version (PyPI, conda, Maven,
    /// Terraform, the path-addressed kinds) answers `None` here, because
    /// warming cannot name the file either.
    pub fn warm_artifact(&self) -> Option<FetchArtifact> {
        match self.fetchable_by_version() {
            FetchSupport::ByVersion(artifact) => Some(artifact),
            FetchSupport::None(_) => None,
        }
    }

    /// The artifact sub-coordinates one version of this kind has, one per
    /// platform — for the two kinds whose "one artifact per version" is
    /// really one per platform (RFC 0010 §6.9).
    ///
    /// `sdkman` and `nodedist` answer [`Self::fetchable_by_version`] with
    /// `None` because the platform is not a constant, so the console's fetch
    /// button cannot name the file. Warming can, once it is *told* the
    /// platforms: `[registries.cache] warm_platforms`, defaulting to the
    /// platform this server runs on ([`Self::host_platform`]). Guessing all
    /// eight SDKMAN platforms would fetch 1.6 GB of JDK to satisfy a one-line
    /// `.sdkmanrc`.
    ///
    /// `None` for every other kind, which keeps [`Self::warm_artifact`] the
    /// single answer for them.
    pub fn platform_artifacts(
        &self,
        name: &str,
        version: &str,
        platforms: &[String],
    ) -> Option<Vec<String>> {
        let host;
        let platforms: &[String] = if platforms.is_empty() {
            host = [self.host_platform()?.to_owned()];
            &host
        } else {
            platforms
        };
        match self {
            // The platform *is* the artifact: `{c}/{v}/{platform}`.
            Self::Sdkman => Some(platforms.to_vec()),
            // The tree's own file names: `node-v22.11.0-linux-x64.tar.xz`,
            // `.zip` on Windows, `iojs-…` on an io.js tree.
            Self::Nodedist => Some(
                platforms
                    .iter()
                    .map(|p| {
                        let ext = if p.starts_with("win") {
                            "zip"
                        } else {
                            "tar.xz"
                        };
                        format!("{name}-{version}-{p}.{ext}")
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The platform this server runs on, spelled the way this kind spells
    /// platforms — the default for `warm_platforms`.
    ///
    /// `None` for the kinds that have no platform axis.
    pub fn host_platform(&self) -> Option<&'static str> {
        use std::env::consts::{ARCH, OS};
        match self {
            Self::Sdkman => Some(match (OS, ARCH) {
                ("linux", "x86_64") => "linuxx64",
                ("linux", "x86") => "linuxx32",
                ("linux", "aarch64") => "linuxarm64",
                ("linux", "arm") => "linuxarm32hf",
                ("macos", "x86_64") => "darwinx64",
                ("macos", "aarch64") => "darwinarm64",
                ("windows", "x86_64") => "windowsx64",
                _ => "exotic",
            }),
            Self::Nodedist => Some(match (OS, ARCH) {
                ("linux", "x86_64") => "linux-x64",
                ("linux", "aarch64") => "linux-arm64",
                ("linux", "arm") => "linux-armv7l",
                ("macos", "x86_64") => "darwin-x64",
                ("macos", "aarch64") => "darwin-arm64",
                ("windows", "x86_64") => "win-x64",
                ("windows", "aarch64") => "win-arm64",
                _ => "linux-x64",
            }),
            _ => None,
        }
    }

    /// The exact `PackageId` a "fetch this version" runs — the same one the
    /// package manager's own download builds, sub-coordinate and normalisation
    /// included.
    ///
    /// `None` when this kind cannot name one artifact for a version; the caller
    /// shows [`FetchSupport::reason`] instead.
    pub fn fetch_coordinate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Option<crate::entities::PackageId> {
        self.warm_artifact()
            .map(|artifact| artifact.coordinate(registry, name, version))
    }
}

/// Whether a version can be fetched by coordinate alone
/// ([`RegistryKind::fetchable_by_version`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSupport {
    /// The coordinate names one artifact, and this is where the download path
    /// caches it.
    ///
    /// There is no payload-less variant on purpose. Every kind that answers
    /// this way addresses its artifact with a sub-coordinate — the bare
    /// `{registry}/{name}/{version}` key is not a slot any read path looks in —
    /// and a variant that let a kind stay silent about which one is what made
    /// the button write dead keys for eight kinds.
    ByVersion(FetchArtifact),
    /// It does not, and this is why. Quoted verbatim by the endpoint and by the
    /// console, so the published support table, the refusal and the button's
    /// tooltip cannot disagree.
    None(&'static str),
}

impl FetchSupport {
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::None(_))
    }

    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::None(reason) => Some(reason),
            Self::ByVersion(_) => None,
        }
    }
}

/// How a kind addresses the one artifact a version resolves to.
///
/// Turned into a `PackageId` by [`Self::coordinate`], which is the only place
/// that answer is built: the console's fetch and the warmer both go through it,
/// so neither can write a key the proxy read path does not read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchArtifact {
    /// A fixed sub-coordinate, the same for every package of this kind —
    /// npm's `tarball`, cargo's `dl`, RubyGems' `gem`, Composer's `dist`,
    /// goproxy's `zip`, `vsix`, `plugin`.
    Fixed(&'static str),
    /// NuGet's flat container addresses the package by filename:
    /// `{LOWER_ID}/{LOWER_VERSION}/{LOWER_ID}.{LOWER_VERSION}.nupkg` (NuGet V3
    /// "package content" resource). Derivable from the coordinate, unlike
    /// PyPI's wheel tags and conda's build strings — which is why NuGet keeps
    /// its button and those two do not.
    ///
    /// The lower-casing is not cosmetic: `nuget restore` requests the
    /// lower-cased URL, so the flat-index handler keys the artifact under the
    /// lower-cased id and version. A fetch that kept `Newtonsoft.Json` as typed
    /// in the console would write a neighbouring key no restore ever reads.
    NugetFlat,
}

impl FetchArtifact {
    /// The `PackageId` — including any normalisation the read path applies —
    /// this artifact is fetched and cached as.
    pub fn coordinate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> crate::entities::PackageId {
        use crate::entities::PackageId;
        match self {
            Self::Fixed(artifact) => {
                PackageId::new(registry, name, version).with_artifact(*artifact)
            }
            Self::NugetFlat => {
                let id = name.to_lowercase();
                let version = version.to_lowercase();
                let filename = format!("{id}.{version}.nupkg");
                PackageId::new(registry, id, version).with_artifact(filename)
            }
        }
    }
}

impl std::str::FromStr for RegistryKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .find(|k| k.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown registry type: '{s}'"))
    }
}

impl std::fmt::Display for RegistryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_round_trips_every_variant() {
        for kind in RegistryKind::ALL {
            let s = kind.as_str();
            assert_eq!(s.parse::<RegistryKind>().unwrap(), *kind);
        }
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert!("not-a-real-type".parse::<RegistryKind>().is_err());
    }

    #[test]
    fn serde_round_trips_kebab_case() {
        for kind in RegistryKind::ALL {
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let back: RegistryKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *kind);
        }
    }

    #[test]
    fn local_mode_support_matches_source_hosting_exclusion() {
        assert!(!RegistryKind::Github.supports_local_mode());
        assert!(!RegistryKind::Forgejo.supports_local_mode());
        assert!(!RegistryKind::Gitlab.supports_local_mode());
        assert!(!RegistryKind::Jetbrains.supports_local_mode());
        assert!(!RegistryKind::Generic.supports_local_mode());
        assert!(!RegistryKind::Nodedist.supports_local_mode());
        assert!(!RegistryKind::Sdkman.supports_local_mode());
        assert!(RegistryKind::Cargo.supports_local_mode());
        assert!(RegistryKind::Deb.supports_local_mode());
        assert!(RegistryKind::JetbrainsMarketplace.supports_local_mode());
    }

    #[test]
    fn only_deb_rpm_and_generic_require_explicit_upstream_in_proxy_mode() {
        assert!(RegistryKind::Deb.requires_explicit_upstream_in_proxy_mode());
        assert!(RegistryKind::Rpm.requires_explicit_upstream_in_proxy_mode());
        assert!(RegistryKind::Generic.requires_explicit_upstream_in_proxy_mode());
        assert!(!RegistryKind::Pacman.requires_explicit_upstream_in_proxy_mode());
        assert!(!RegistryKind::Npm.requires_explicit_upstream_in_proxy_mode());
        // `https://nodejs.org/dist` is the default the client itself uses.
        assert!(!RegistryKind::Nodedist.requires_explicit_upstream_in_proxy_mode());
        // `https://api.sdkman.io/2` is the default `sdkman-init.sh` sets.
        assert!(!RegistryKind::Sdkman.requires_explicit_upstream_in_proxy_mode());
        assert!(!RegistryKind::JetbrainsMarketplace.requires_explicit_upstream_in_proxy_mode());
    }

    #[test]
    fn every_kind_answers_the_listing_filter_question() {
        for kind in RegistryKind::ALL {
            let docs = kind.listing_filter();
            if matches!(kind, RegistryKind::Generic | RegistryKind::Jetbrains) {
                assert!(
                    docs.is_empty(),
                    "{kind} is path-addressed and has no listing document"
                );
                continue;
            }
            assert!(
                !docs.is_empty(),
                "{kind} names no listing document; if it genuinely has none, say so here"
            );
            for d in docs {
                assert!(!d.label.is_empty(), "{kind}: a document needs a name");
                if let ListingSupport::Unsupported(reason) | ListingSupport::Qualified(reason) =
                    d.support
                {
                    assert!(
                        !reason.is_empty(),
                        "{kind}/{}: the published table prints this reason verbatim",
                        d.label
                    );
                }
            }
        }
    }

    /// Every "no" row of the coverage table, and only those. Each one is a
    /// deliberate decision with a reason an operator can read, not an omission.
    #[test]
    fn the_unfiltered_documents_are_the_ones_we_decided_not_to_filter() {
        let unfiltered: Vec<&str> = RegistryKind::ALL
            .iter()
            .filter(|k| {
                k.listing_filter()
                    .iter()
                    .any(|d| matches!(d.support, ListingSupport::Unsupported(_)))
            })
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            unfiltered,
            [
                // Marshal indexes only; the JSON APIs are filtered.
                "rubygems", // Signed repository indexes.
                "deb", "rpm", "pacman",
            ]
        );
    }

    /// Every kind answers, and a "no" answers with a reason an operator can
    /// read — the published support table prints it verbatim.
    #[test]
    fn every_kind_answers_the_readme_question_with_a_reason() {
        for kind in RegistryKind::ALL {
            if let ReadmeSupport::None(reason) = kind.readme_support() {
                assert!(
                    !reason.is_empty(),
                    "{kind} says it has no README; say why, the docs table quotes it"
                );
            }
        }
    }

    /// The three "no" groups of RFC 0007 §4.3, and only those. Each is a
    /// decision in §3's non-goals, not a gap to be closed later.
    #[test]
    fn the_kinds_without_a_readme_are_the_ones_we_decided_against() {
        let unsupported: Vec<&str> = RegistryKind::ALL
            .iter()
            .filter(|k| !k.readme_support().is_supported())
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            unsupported,
            [
                // The README is a file already served by path.
                "github",
                "forgejo",
                "gitlab",
                // A sentence, not a document.
                "maven",
                // Path-addressed: no package identity.
                "deb",
                "rpm",
                "pacman",
                "jetbrains",
                "generic",
                // Tarballs and a checksum file: no prose anywhere in the tree.
                "nodedist",
                // A distribution, not a package: no document carries prose.
                "sdkman",
            ]
        );
    }

    /// A kind that answers for unheld versions must not be one whose README
    /// only exists inside bytes we do not have — that combination would put
    /// "available" on a row we can never serve.
    #[test]
    fn only_metadata_borne_kinds_answer_for_unheld_versions() {
        assert!(RegistryKind::Npm
            .readme_support()
            .answers_for_unheld_versions());
        assert!(RegistryKind::Pypi
            .readme_support()
            .answers_for_unheld_versions());
        assert!(RegistryKind::Openvsx
            .readme_support()
            .answers_for_unheld_versions());
        assert!(!RegistryKind::Cargo
            .readme_support()
            .answers_for_unheld_versions());
        assert!(!RegistryKind::Maven
            .readme_support()
            .answers_for_unheld_versions());
        // npm is both: the packument answers when it carries the text, the
        // tarball when it does not.
        assert!(RegistryKind::Npm.readme_support().reads_the_archive());
        // A kind whose README arrives in a *link* has nothing in the archive to
        // fall back to, and must not claim otherwise.
        assert!(!RegistryKind::Openvsx.readme_support().reads_the_archive());
    }

    /// Every kind answers, and a "no" answers with a reason the published
    /// table prints verbatim.
    #[test]
    fn every_kind_answers_the_upstream_detail_question_with_a_reason() {
        for kind in RegistryKind::ALL {
            if let UpstreamDetailSupport::None(reason) = kind.upstream_detail() {
                assert!(
                    !reason.is_empty(),
                    "{kind} says it cannot be asked about upstream; say why"
                );
            }
        }
    }

    /// A kind whose README arrives in a metadata document must be one the
    /// discovery read can actually reach, or the support table promises a
    /// README for an unheld version that nothing can fetch.
    #[test]
    fn every_kind_that_answers_for_unheld_versions_can_be_asked_upstream() {
        for kind in RegistryKind::ALL {
            if kind.readme_support().answers_for_unheld_versions() {
                assert!(
                    kind.upstream_detail().is_supported(),
                    "{kind} promises a README for unheld versions but cannot be asked upstream"
                );
            }
        }
    }

    #[test]
    fn path_addressed_kinds_are_the_path_proxy_ones() {
        for kind in [
            RegistryKind::Deb,
            RegistryKind::Rpm,
            RegistryKind::Pacman,
            RegistryKind::Jetbrains,
            RegistryKind::Generic,
        ] {
            assert!(kind.is_path_addressed(), "{kind} should be path-addressed");
        }
        for kind in [
            RegistryKind::Npm,
            RegistryKind::Cargo,
            RegistryKind::Github,
            RegistryKind::Goproxy,
            RegistryKind::JetbrainsMarketplace,
            // Typed, not path-addressed, and that is the whole point of the
            // kind: `generic` mirrors the same tree and can block nothing on it.
            RegistryKind::Nodedist,
            RegistryKind::Sdkman,
        ] {
            assert!(
                !kind.is_path_addressed(),
                "{kind} should not be path-addressed"
            );
        }
    }

    /// The cache key a console fetch writes, spelled out per kind.
    ///
    /// This is the regression: the table used to answer "by version, no
    /// sub-coordinate" for eight kinds, so the fetch wrote
    /// `npm1/lodash/4.17.21` while `npm install` reads
    /// `npm1/lodash/4.17.21/tarball` — a success that filled a slot nothing
    /// reads. Each expectation below is the key the *download handler* for that
    /// kind builds (`handlers/proxy/<kind>`, via `artifact_suffix`), so a change
    /// to either side has to change this list too.
    #[test]
    fn a_fetch_writes_the_key_the_download_path_reads() {
        let cases = [
            (RegistryKind::Npm, "r/lodash/4.17.21/tarball"),
            (RegistryKind::Cargo, "r/lodash/4.17.21/dl"),
            (RegistryKind::Composer, "r/lodash/4.17.21/dist"),
            (RegistryKind::Rubygems, "r/lodash/4.17.21/gem"),
            (RegistryKind::Goproxy, "r/lodash/4.17.21/zip"),
            (RegistryKind::Openvsx, "r/lodash/4.17.21/vsix"),
            (RegistryKind::VscodeMarketplace, "r/lodash/4.17.21/vsix"),
            (
                RegistryKind::JetbrainsMarketplace,
                "r/lodash/4.17.21/plugin",
            ),
        ];
        for (kind, expected) in cases {
            let pkg = kind
                .fetch_coordinate("r", "lodash", "4.17.21")
                .unwrap_or_else(|| panic!("{kind} should be fetchable by version"));
            assert_eq!(pkg.cache_key(), expected, "{kind}");
        }
    }

    /// NuGet's flat container is addressed by a lower-cased filename, and
    /// `nuget restore` requests the lower-cased URL — so a fetch of the
    /// mixed-case id the console displays has to normalise, or it writes a
    /// neighbouring key no restore reads.
    #[test]
    fn a_nuget_fetch_normalises_the_way_the_flat_index_does() {
        let pkg = RegistryKind::Nuget
            .fetch_coordinate("r", "Newtonsoft.Json", "13.0.3-Beta")
            .unwrap();
        assert_eq!(
            pkg.cache_key(),
            "r/newtonsoft.json/13.0.3-beta/newtonsoft.json.13.0.3-beta.nupkg"
        );
    }

    /// PyPI and conda name a *set* of files per version, so the button is
    /// refused with a reason rather than offered and quietly wrong. Left
    /// offered, PyPI looked for a file named `""` and 404'd, and conda —
    /// which carries the channel platform in the version slot — asked for
    /// `{base}/{version}/repodata.json`, a path that cannot exist.
    #[test]
    fn kinds_that_cannot_name_one_artifact_refuse_with_a_reason() {
        for kind in [
            RegistryKind::Pypi,
            RegistryKind::Conda,
            RegistryKind::Maven,
            RegistryKind::Terraform,
            RegistryKind::Nodedist,
            RegistryKind::Sdkman,
        ] {
            assert!(
                kind.fetchable_by_version().reason().is_some(),
                "{kind} should refuse with a reason"
            );
            assert!(
                kind.fetch_coordinate("r", "numpy", "1.24.0").is_none(),
                "{kind} should not produce a fetch coordinate"
            );
        }
    }

    /// A block on a JDK means all eight platforms (RFC 0010 decision 8): the
    /// listing coordinate carries the platform, the blocked-set lookup must
    /// not. Inert for every other kind.
    #[test]
    fn the_blocking_name_drops_sdkmans_platform_and_nothing_else() {
        assert_eq!(
            RegistryKind::Sdkman.blocking_package_name("java/linuxx64"),
            "java"
        );
        assert_eq!(
            RegistryKind::Sdkman.blocking_package_name("java/linuxx64?current=&installed="),
            "java"
        );
        assert_eq!(RegistryKind::Sdkman.blocking_package_name("java"), "java");
        for kind in RegistryKind::ALL
            .iter()
            .filter(|k| **k != RegistryKind::Sdkman)
        {
            assert_eq!(kind.blocking_package_name("a/b?c"), "a/b?c", "{kind}");
        }
    }

    /// The two platform-addressed kinds warm one file per platform, spelled
    /// as the tree and the broker spell them; everything else has no
    /// platform axis and answers `None` here as it does for `warm_artifact`.
    #[test]
    fn platform_artifacts_name_one_file_per_platform_for_the_toolchain_kinds() {
        let platforms = ["linux-x64".to_owned(), "win-x64".to_owned()];
        assert_eq!(
            RegistryKind::Nodedist.platform_artifacts("node", "v22.11.0", &platforms),
            Some(vec![
                "node-v22.11.0-linux-x64.tar.xz".to_owned(),
                "node-v22.11.0-win-x64.zip".to_owned(),
            ])
        );
        let platforms = ["linuxx64".to_owned(), "darwinarm64".to_owned()];
        assert_eq!(
            RegistryKind::Sdkman.platform_artifacts("java", "21.0.5-tem", &platforms),
            Some(vec!["linuxx64".to_owned(), "darwinarm64".to_owned()])
        );
        // No platforms given: the host's own, which is never empty.
        let host = RegistryKind::Sdkman
            .platform_artifacts("java", "21.0.5-tem", &[])
            .unwrap();
        assert_eq!(host.len(), 1);
        assert!(crate::services::sdkman::parse_platform(&host[0]).is_some());
        for kind in RegistryKind::ALL
            .iter()
            .filter(|k| !matches!(k, RegistryKind::Sdkman | RegistryKind::Nodedist))
        {
            assert_eq!(
                kind.platform_artifacts("x", "1.0.0", &platforms),
                None,
                "{kind}"
            );
            assert_eq!(kind.host_platform(), None, "{kind}");
        }
    }

    /// Warming and the console fetch must not be two lists of the same fact.
    #[test]
    fn warming_and_fetching_agree_on_every_kind() {
        for kind in RegistryKind::ALL {
            match kind.fetchable_by_version() {
                FetchSupport::ByVersion(artifact) => {
                    assert_eq!(kind.warm_artifact(), Some(artifact), "{kind}")
                }
                FetchSupport::None(_) => assert_eq!(kind.warm_artifact(), None, "{kind}"),
            }
        }
    }
}
