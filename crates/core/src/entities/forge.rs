//! What a "version" is on a git forge (RFC 0019 §4.2).
//!
//! A package registry names immutable things: `lodash@4.17.21` is one tarball
//! forever. A forge does not. `cli/cli@main` is whatever `main` points at when
//! you ask, and `v2.60.0` is a tag that can be moved. So a forge coordinate is
//! parsed into its *kind* and its *ref*, and the ref is resolved to a commit
//! SHA before anything is fetched — that SHA is what the cache, the metadata
//! and (with RFC 0018) the verdict key on.
//!
//! The URL scheme does not change. The handlers already build `PackageId`s
//! with a small set of artifact conventions (`tarball/{ref}`, `zipball`,
//! `raw/{path}`, `filename/{name}`, an asset id, and `version = "releases"`
//! for the listing); [`ForgeCoordinate::from_package_id`] reads those
//! conventions back rather than inventing a second address.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::PackageId;

/// What kind of ref a request named, once resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    /// A commit SHA. Immutable by construction; resolved without a call.
    Commit,
    /// A tag. Immutable by convention — a moved tag is a finding, not a
    /// version (RFC 0019 phase 2).
    Tag,
    /// A branch. Mutable by design: served by following it, and every such
    /// response says so.
    Branch,
}

impl RefKind {
    /// The wire spelling — `X-BatleHub-Ref-Kind`, `extra.forge.ref_kind`, and
    /// the `ref_kind` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tag => "tag",
            Self::Branch => "branch",
        }
    }
}

impl std::str::FromStr for RefKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "commit" => Ok(Self::Commit),
            "tag" => Ok(Self::Tag),
            "branch" => Ok(Self::Branch),
            other => Err(format!("unknown ref kind '{other}'")),
        }
    }
}

impl std::fmt::Display for RefKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `s` is a full 40-hex commit SHA.
///
/// Only the full form resolves without a call: an abbreviated SHA is
/// unambiguous on the forge but not here, and treating `deadbeef` as a commit
/// would let a branch of that name skip resolution.
pub fn is_commit_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The kind of thing a forge request names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeKind {
    /// `GET /{o}/{r}/releases` — the listing document.
    Listing,
    /// `GET /{o}/{r}/releases/tags/{tag}` — one release's JSON.
    Release { tag: String },
    /// A release asset, by id or by `filename/{name}`, under `tag`.
    Asset { tag: String, selector: String },
    /// `tarball/{ref}` or `zipball` — a forge-generated archive of `git_ref`.
    Archive {
        git_ref: String,
        format: ArchiveFormat,
    },
    /// `raw/{path}` at `git_ref`.
    Raw { git_ref: String, path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarGz,
    Zip,
}

/// A forge request, read back from the `PackageId` the handler built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeCoordinate {
    /// `owner/repo`, as the handler joined it.
    pub owner_repo: String,
    pub kind: ForgeKind,
}

impl ForgeCoordinate {
    /// Read the handler's conventions back. `None` for a coordinate that is
    /// not a forge read at all — the Forgejo packages passthrough
    /// (`pkgpath/…`), which addresses no repository.
    pub fn from_package_id(pkg: &PackageId) -> Option<Self> {
        let owner_repo = pkg.name.clone();
        let kind = match pkg.artifact.as_deref() {
            None if pkg.version == "releases" => ForgeKind::Listing,
            None => ForgeKind::Release {
                tag: pkg.version.clone(),
            },
            Some(a) if a.starts_with("pkgpath/") => return None,
            Some(a) if a.starts_with("tarball/") => ForgeKind::Archive {
                git_ref: pkg.version.clone(),
                format: ArchiveFormat::TarGz,
            },
            Some("zipball") => ForgeKind::Archive {
                git_ref: pkg.version.clone(),
                format: ArchiveFormat::Zip,
            },
            // GitLab spells the same read `rawfile/{git_ref}/{path}` — its
            // route carries the ref in the path as well as in the version — so
            // it has to be recognised here too. Without it a GitLab raw read
            // fell through to `Asset`, and `[registries.raw]` (the policy rule,
            // the shebang refusal and the size ceiling all key on
            // `ForgeKind::Raw`) was inert on GitLab alone.
            Some(a) if a.starts_with("rawfile/") => {
                let rest = &a["rawfile/".len()..];
                let path = rest
                    .strip_prefix(&format!("{}/", pkg.version))
                    .unwrap_or(rest);
                ForgeKind::Raw {
                    git_ref: pkg.version.clone(),
                    path: path.to_owned(),
                }
            }
            Some(a) => match a.strip_prefix("raw/") {
                Some(path) => ForgeKind::Raw {
                    git_ref: pkg.version.clone(),
                    path: path.to_owned(),
                },
                None => ForgeKind::Asset {
                    tag: pkg.version.clone(),
                    selector: a.to_owned(),
                },
            },
        };
        Some(Self { owner_repo, kind })
    }

    /// The ref this coordinate names, when it names one that is resolved.
    ///
    /// Archives and raw files: the ref decides the bytes, so it is resolved
    /// and the cache is keyed on the commit. Releases and assets: the tag is
    /// resolved too, so a moved tag is recorded (phase 2 turns that into a
    /// finding), but the cache keeps the tag as its key — the uploaded bytes,
    /// not the commit, are an asset's identity (RFC 0019 §4.2 *Identity*).
    /// The listing names no ref.
    pub fn git_ref(&self) -> Option<&str> {
        match &self.kind {
            ForgeKind::Listing => None,
            // An asset addressed by id alone carries the placeholder, not a
            // ref; there is nothing to resolve until the asset says which
            // release it belongs to, and that is the client's lookup.
            ForgeKind::Asset { tag, .. } if tag == UNKNOWN_TAG => None,
            ForgeKind::Release { tag } | ForgeKind::Asset { tag, .. } => Some(tag),
            ForgeKind::Archive { git_ref, .. } | ForgeKind::Raw { git_ref, .. } => Some(git_ref),
        }
    }

    /// Whether the resolved commit becomes the cache key (`version := sha`).
    pub fn keyed_by_commit(&self) -> bool {
        matches!(self.kind, ForgeKind::Archive { .. } | ForgeKind::Raw { .. })
    }

    /// The coordinate rewritten onto its resolved commit, for the cache and
    /// the client: `version` becomes `sha`, and a `tarball/{ref}` artifact
    /// becomes `tarball/{sha}` so the two agree.
    ///
    /// Not applied to releases and assets — see [`Self::keyed_by_commit`].
    pub fn rewrite_onto(&self, pkg: &PackageId, sha: &str) -> PackageId {
        if !self.keyed_by_commit() {
            return pkg.clone();
        }
        let artifact = match &self.kind {
            ForgeKind::Archive {
                format: ArchiveFormat::TarGz,
                ..
            } => Some(format!("tarball/{sha}")),
            _ => pkg.artifact.clone(),
        };
        PackageId {
            registry: pkg.registry.clone(),
            name: pkg.name.clone(),
            version: sha.to_owned(),
            artifact,
        }
    }
}

/// A ref, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRef {
    /// The ref as the client spelled it — `main`, `v2.60.0`, a SHA.
    pub requested: String,
    pub kind: RefKind,
    /// The commit it resolved to. For an annotated tag this is the commit the
    /// tag object points at, not the tag object itself.
    pub sha: String,
    pub resolved_at: DateTime<Utc>,
    /// The commit this ref resolved to the previous time, when it differs —
    /// a moved tag or an advanced branch. Phase 2's `TAG_MOVED` reads this.
    pub previous: Option<String>,
    /// When the forge says the object was made: the tagger date for an
    /// annotated tag, the committer date otherwise. `None` when the
    /// resolution came from the local table rather than the forge.
    pub object_date: Option<DateTime<Utc>>,
    /// Who the forge names for it: the tagger, or the committer.
    pub publisher: Option<String>,
}

impl ResolvedRef {
    /// Whether this resolution came back different from the last one.
    pub fn moved(&self) -> bool {
        self.previous.is_some()
    }
}

/// How long a resolution is trusted before the forge is asked again
/// (`[registries.refs]`, RFC 0019 §4.1).
/// What a forge-ref finding does to the request (RFC 0019 §4.2).
///
/// `Warn` serves the bytes and says so — the response is `warned` where a
/// verdict exists, and carries the ref headers where none does. `Deny`
/// refuses. The default differs per code and is the RFC's: a branch is
/// mutable *by design*, so following it is warned; a tag that moved is
/// either a mistake or an attack, so it is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefAction {
    Warn,
    Deny,
}

impl RefAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Deny => "deny",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "warn" => Some(Self::Warn),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// The severity the finding carries, which is how the verdict learns
    /// what the operator chose: `deny` arrives as `critical`, `warn` as
    /// `low`. RFC 0018's `classify` reads it and never drops either — a
    /// warned ref is visible by construction.
    pub fn severity(&self) -> super::Severity {
        match self {
            Self::Warn => super::Severity::Low,
            Self::Deny => super::Severity::Critical,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForgeRefsPolicy {
    pub branch_ttl: std::time::Duration,
    pub tag_ttl: std::time::Duration,
    /// Following a branch (`MUTABLE_REF`). Warned by default: serving `main`
    /// is legitimate, serving it silently is not.
    pub mutable_refs: RefAction,
    /// A tag whose commit changed since it was first resolved (`TAG_MOVED`),
    /// and a release asset whose digest changed (`ASSET_REPLACED`). Denied by
    /// default: these are the two forge-native ways to swap bytes under a
    /// stable coordinate.
    pub tag_moved: RefAction,
    /// Never re-resolve: a stored resolution is fresh however old it is.
    ///
    /// Set from `air_gap.enabled` rather than from `[registries.refs]`
    /// (RFC 0008 §13.3). A TTL is a promise to go and ask again, and an
    /// air-gapped instance has nobody to ask — so an expired row would fall
    /// through to a refusal and take every cached forge artifact with it,
    /// because a ref is resolved before anything is fetched. Frozen, the
    /// stored resolution is simply what this instance knows, which is what
    /// the bundle gave it.
    pub frozen: bool,
}

impl Default for ForgeRefsPolicy {
    /// Branches re-resolved every minute, tags every hour — the RFC's
    /// defaults. A commit is never re-resolved; it cannot move.
    fn default() -> Self {
        Self {
            branch_ttl: std::time::Duration::from_secs(60),
            tag_ttl: std::time::Duration::from_secs(3600),
            mutable_refs: RefAction::Warn,
            tag_moved: RefAction::Deny,
            frozen: false,
        }
    }
}

impl ForgeRefsPolicy {
    pub fn ttl_for(&self, kind: RefKind) -> Option<std::time::Duration> {
        match kind {
            RefKind::Commit => None,
            RefKind::Tag => Some(self.tag_ttl),
            RefKind::Branch => Some(self.branch_ttl),
        }
    }
}

/// What a forge can say about who made a thing and whether that is
/// verifiable (RFC 0019 §4.2 *Metadata contract*, phase 5).
///
/// Four answers, and the difference between the last three matters:
/// *missing* means the forge has nothing, *unverifiable* means it has
/// something that cannot be checked cryptographically (GitLab's release
/// evidence, and only GitLab's — decision 8), *invalid* means it has a
/// signature that does not verify. Only the first is good news, and none of
/// them is a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ForgeProvenance {
    /// A signature or attestation the forge verified, with what it says.
    Verified { detail: String },
    /// A signature the forge could not verify.
    Invalid { reason: String },
    /// Evidence exists but cannot be cryptographically checked. **GitLab
    /// only**; the GitHub and Forgejo clients have a test asserting they
    /// never produce it.
    Unverifiable { detail: String },
    /// The forge has nothing for this object.
    Missing,
}

impl ForgeProvenance {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Verified { .. } => "verified",
            Self::Invalid { .. } => "invalid",
            Self::Unverifiable { .. } => "unverifiable",
            Self::Missing => "missing",
        }
    }

    /// The reason code this state produces, or `None` when there is nothing
    /// to say — a verified signature is the case that needs no finding.
    pub fn reason_code(&self) -> Option<super::ReasonCode> {
        match self {
            Self::Verified { .. } => None,
            Self::Invalid { .. } => Some(super::ReasonCode::ProvenanceInvalid),
            Self::Unverifiable { .. } => Some(super::ReasonCode::ProvenanceUnverifiable),
            Self::Missing => Some(super::ReasonCode::ProvenanceMissing),
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Verified { detail } | Self::Unverifiable { detail } => detail,
            Self::Invalid { reason } => reason,
            Self::Missing => "the forge has no signature or attestation for this object",
        }
    }
}

/// The `extra.forge` key provenance travels under until RFC 0018 grows a
/// typed field for it.
pub const FORGE_PROVENANCE: &str = "provenance";

/// What a raw file that looks like a script does (RFC 0019 §4.2 *Raw
/// content*, phase 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptAction {
    /// Served, and the response says `RAW_SCRIPT`.
    Warn,
    /// Refused. On a registry with `[registries.security]` this is the
    /// default: opting into a quarantine is opting into "nothing unscanned
    /// is served", and a shell script passed through is the one artifact no
    /// scanner here reads (RFC 0019 §11 q2, decided 2026-09-04).
    Deny,
    /// Not looked at.
    Ignore,
}

impl ScriptAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Deny => "deny",
            Self::Ignore => "ignore",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "warn" => Some(Self::Warn),
            "deny" => Some(Self::Deny),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }
}

/// `[registries.raw]` as the read path reads it (RFC 0019 §4.1).
///
/// **Absent means off.** Raw was implicitly on for all three forges before
/// this policy existed; an operator who relies on it writes `enabled = true`,
/// and the first refused request says so in its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPolicy {
    pub enabled: bool,
    /// Never zero when enabled: validation refuses that, because unbounded
    /// raw is the hole the section closes.
    pub max_size_bytes: u64,
    /// `owner/repo` globs; empty means any repository.
    pub repos: Vec<String>,
    /// Refuse a branch ref: raw content that can change under the same URL
    /// is what a pinned estate does not want.
    pub require_pinned: bool,
    pub scripts: ScriptAction,
}

impl Default for RawPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_size_bytes: 10 * 1024 * 1024,
            repos: Vec::new(),
            require_pinned: false,
            scripts: ScriptAction::Warn,
        }
    }
}

impl RawPolicy {
    /// Whether `owner_repo` is inside the allowlist. An empty list is every
    /// repository — the list narrows, it never widens.
    pub fn allows_repo(&self, owner_repo: &str) -> bool {
        if self.repos.is_empty() {
            return true;
        }
        self.repos
            .iter()
            .any(|pattern| glob_match(pattern, owner_repo))
    }

    /// Whether the path names a script by its extension. The cheap half of
    /// the two §4.2 asks for; the shebang is read from the first bytes on the
    /// read path, where they exist.
    pub fn looks_like_script(path: &str) -> bool {
        let lower = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
        SCRIPT_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
    }

    /// Whether the first bytes of a file are a shebang or a PowerShell/batch
    /// preamble. Read on the read path from the first chunk, so a payload
    /// with no extension is still caught when the action is `deny`.
    pub fn starts_like_script(first_bytes: &[u8]) -> bool {
        let head = &first_bytes[..first_bytes.len().min(512)];
        if head.starts_with(b"#!") {
            return true;
        }
        let text = String::from_utf8_lossy(head).to_ascii_lowercase();
        text.starts_with("@echo off")
            || text.starts_with("<#")
            || text.contains("param(") && text.contains("$psscriptroot")
    }
}

/// The extensions §4.2 names: shell, PowerShell, Python, batch.
pub const SCRIPT_EXTENSIONS: &[&str] = &[
    ".sh", ".bash", ".zsh", ".ksh", ".ps1", ".psm1", ".py", ".bat", ".cmd",
];

/// `owner/repo` glob: `*` matches within one path segment, as the RFC's
/// `cli/*` example means. Not a general glob — a pattern is two segments,
/// each either a literal or `*`.
fn glob_match(pattern: &str, value: &str) -> bool {
    let (p_owner, p_repo) = match pattern.split_once('/') {
        Some(pair) => pair,
        None => (pattern, "*"),
    };
    let Some((v_owner, v_repo)) = value.split_once('/') else {
        return false;
    };
    let seg = |p: &str, v: &str| p == "*" || p.eq_ignore_ascii_case(v);
    seg(p_owner, v_owner) && seg(p_repo, v_repo)
}

/// The typed read-only JSON families `[registries.api_reads]` can add
/// (RFC 0019 §4.1). Closed on purpose: `contents` and `git/blobs` are raw by
/// another door, and a wildcard passthrough would proxy writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiReadFamily {
    Tags,
    Commits,
    Branches,
}

impl ApiReadFamily {
    pub const ALL: [ApiReadFamily; 3] = [Self::Tags, Self::Commits, Self::Branches];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tags => "tags",
            Self::Commits => "commits",
            Self::Branches => "branches",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tags" => Some(Self::Tags),
            "commits" => Some(Self::Commits),
            "branches" => Some(Self::Branches),
            _ => None,
        }
    }
}

/// `extra.forge` keys phase 2 reads back — written by
/// `overlay_forge_metadata` (the resolution) and by the clients (the asset
/// digest). Named constants because the rule, the scanner and the tests all
/// spell them, and a typo in one of the three is a gate that never fires.
pub const FORGE_REF_KIND: &str = "ref_kind";
pub const FORGE_RESOLVED_COMMIT: &str = "resolved_commit";
pub const FORGE_PREVIOUS_COMMIT: &str = "previous_commit";
pub const FORGE_REQUESTED_REF: &str = "requested_ref";
/// The digest the forge advertises for a release asset, `sha256:…` — GitHub
/// carries it on the asset JSON (confirmed 2026-09-04: `digest`, null on
/// assets uploaded before the field existed). Compared with the digest of
/// the bytes already cached under the coordinate: a difference is
/// `ASSET_REPLACED`.
pub const FORGE_ASSET_DIGEST: &str = "asset_digest";

/// The key `extra` carries forge facts under (RFC 0019 §4.2 *Metadata
/// contract*), until RFC 0018's typed fields exist.
pub const FORGE_EXTRA_KEY: &str = "forge";

/// The tag the asset-by-id route (`/releases/assets/{id}`) carries when the
/// client sent no `?tag=` — which is every real client: mise and `gh` both
/// address an asset by its id alone. It names no ref, so nothing resolves it;
/// the client derives the release from the asset instead.
pub const UNKNOWN_TAG: &str = "unknown";

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(version: &str, artifact: Option<&str>) -> PackageId {
        let p = PackageId::new("gh", "cli/cli", version);
        match artifact {
            Some(a) => p.with_artifact(a),
            None => p,
        }
    }

    #[test]
    fn a_full_sha_is_a_commit_and_nothing_shorter_is() {
        assert!(is_commit_sha("45437bc7eeeb3359bbfddd1742f79de7652fd3e2"));
        assert!(!is_commit_sha("45437bc"));
        assert!(!is_commit_sha("main"));
        assert!(!is_commit_sha("45437bc7eeeb3359bbfddd1742f79de7652fd3eZ"));
    }

    #[test]
    fn the_handler_conventions_read_back_as_kinds() {
        let listing = ForgeCoordinate::from_package_id(&pkg("releases", None)).unwrap();
        assert_eq!(listing.kind, ForgeKind::Listing);
        assert_eq!(listing.git_ref(), None);

        let release = ForgeCoordinate::from_package_id(&pkg("v2.60.0", None)).unwrap();
        assert_eq!(
            release.kind,
            ForgeKind::Release {
                tag: "v2.60.0".into()
            }
        );
        assert_eq!(release.git_ref(), Some("v2.60.0"));
        assert!(!release.keyed_by_commit());

        let asset =
            ForgeCoordinate::from_package_id(&pkg("v2.60.0", Some("filename/gh.tar.gz"))).unwrap();
        assert!(matches!(asset.kind, ForgeKind::Asset { .. }));
        let by_id = ForgeCoordinate::from_package_id(&pkg("v2.60.0", Some("12345"))).unwrap();
        assert!(matches!(by_id.kind, ForgeKind::Asset { ref selector, .. } if selector == "12345"));
        // By id with no tag: the placeholder is not a ref.
        let by_id_alone =
            ForgeCoordinate::from_package_id(&pkg(UNKNOWN_TAG, Some("12345"))).unwrap();
        assert_eq!(by_id_alone.git_ref(), None);

        let tarball = ForgeCoordinate::from_package_id(&pkg("main", Some("tarball/main"))).unwrap();
        assert!(matches!(
            tarball.kind,
            ForgeKind::Archive {
                format: ArchiveFormat::TarGz,
                ..
            }
        ));
        assert!(tarball.keyed_by_commit());

        let raw = ForgeCoordinate::from_package_id(&pkg("main", Some("raw/install.sh"))).unwrap();
        assert!(matches!(raw.kind, ForgeKind::Raw { ref path, .. } if path == "install.sh"));
        assert_eq!(raw.git_ref(), Some("main"));

        // GitLab's spelling of the same read, which its handler builds as
        // `rawfile/{git_ref}/{path}`. It has to land on `Raw` too: `Asset` puts
        // it outside `[registries.raw]` entirely — no `RawPolicyRule`, no
        // shebang refusal, no size ceiling — on GitLab alone.
        let gl = ForgeCoordinate::from_package_id(&pkg("main", Some("rawfile/main/install.sh")))
            .unwrap();
        assert!(
            matches!(gl.kind, ForgeKind::Raw { ref path, .. } if path == "install.sh"),
            "{:?}",
            gl.kind
        );
        assert_eq!(gl.git_ref(), Some("main"));
        // A nested path keeps every segment below the ref.
        let nested =
            ForgeCoordinate::from_package_id(&pkg("main", Some("rawfile/main/ci/setup.sh")))
                .unwrap();
        assert!(matches!(nested.kind, ForgeKind::Raw { ref path, .. } if path == "ci/setup.sh"));

        assert!(ForgeCoordinate::from_package_id(&pkg(
            "_",
            Some("pkgpath/api/packages/acme/generic/t/1/t.bin")
        ))
        .is_none());
    }

    #[test]
    fn rewriting_onto_a_commit_moves_the_version_and_the_tarball_artifact() {
        let sha = "45437bc7eeeb3359bbfddd1742f79de7652fd3e2";
        let tarball = pkg("main", Some("tarball/main"));
        let c = ForgeCoordinate::from_package_id(&tarball).unwrap();
        let rewritten = c.rewrite_onto(&tarball, sha);
        assert_eq!(
            rewritten.cache_key(),
            format!("gh/cli/cli/{sha}/tarball/{sha}")
        );

        let raw = pkg("main", Some("raw/install.sh"));
        let c = ForgeCoordinate::from_package_id(&raw).unwrap();
        assert_eq!(
            c.rewrite_onto(&raw, sha).cache_key(),
            format!("gh/cli/cli/{sha}/raw/install.sh")
        );

        // Assets keep their key: the uploaded bytes are the identity.
        let asset = pkg("v2.60.0", Some("filename/gh.tar.gz"));
        let c = ForgeCoordinate::from_package_id(&asset).unwrap();
        assert_eq!(c.rewrite_onto(&asset, sha), asset);
    }

    #[test]
    fn ref_kinds_round_trip_their_wire_spelling() {
        for kind in [RefKind::Commit, RefKind::Tag, RefKind::Branch] {
            assert_eq!(kind.as_str().parse::<RefKind>().unwrap(), kind);
        }
        assert!("origin".parse::<RefKind>().is_err());
    }

    #[test]
    fn a_commit_is_never_re_resolved() {
        let p = ForgeRefsPolicy::default();
        assert_eq!(p.ttl_for(RefKind::Commit), None);
        assert_eq!(
            p.ttl_for(RefKind::Branch),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(
            p.ttl_for(RefKind::Tag),
            Some(std::time::Duration::from_secs(3600))
        );
    }
}
