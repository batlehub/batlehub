use serde::Deserialize;

// ── Serde types for GitHub API responses ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(super) struct GhRelease {
    pub id: u64,
    pub tag_name: String,
    pub published_at: Option<String>,
    /// Not published: visible only to those who can edit the repository, and
    /// never imported (RFC 0021 §4.2). `#[serde(default)]` because the field is
    /// absent from a release object embedded elsewhere, and a missing flag must
    /// read as "not a draft" rather than fail the whole decode — this model is
    /// strict, and a required field it did not need has broken a real client
    /// before.
    #[serde(default)]
    pub draft: bool,
    /// Published and marked as not the default download. `latest` skips it;
    /// a tag or `releases = "all"` reaches it.
    #[serde(default)]
    pub prerelease: bool,
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GhAsset {
    pub id: u64,
    pub name: String,
    pub browser_download_url: String,
    #[allow(dead_code)]
    pub size: u64,
    /// `sha256:…` of the uploaded bytes (RFC 0019 §4.2 *Identity of the
    /// bytes*). Confirmed against api.github.com on 2026-09-04: the field is
    /// present on the asset object and is `null` for assets uploaded before
    /// GitHub started recording it, so it is an `Option` and its absence
    /// disables `ASSET_REPLACED` for that asset rather than asserting
    /// anything.
    #[serde(default)]
    pub digest: Option<String>,
}

// ── Refs, tags, branches and commits (RFC 0019) ───────────────────────────────
//
// Shapes confirmed against api.github.com on 2026-09-03: `git/ref/tags/{tag}`
// (`object.type` is `commit` for a lightweight tag, `tag` for an annotated
// one), `git/tags/{sha}` (the tag object, with `tagger` and its own `object`),
// `branches/{name}` and `commits/{sha}`.

/// One person as the git object records them: `name`, `email`, `date`.
#[derive(Debug, Deserialize)]
pub struct GhGitPerson {
    pub name: Option<String>,
    pub email: Option<String>,
    pub date: Option<String>,
}

/// A GitHub account, where the API attaches one.
#[derive(Debug, Deserialize)]
pub struct GhUser {
    pub login: Option<String>,
}

/// The target of a ref or a tag object.
#[derive(Debug, Deserialize)]
pub struct GhObject {
    pub sha: String,
    #[serde(rename = "type")]
    pub kind: String,
}

/// `GET /repos/{o}/{r}/git/ref/tags/{tag}`.
#[derive(Debug, Deserialize)]
pub struct GhRef {
    pub object: GhObject,
}

/// `GET /repos/{o}/{r}/git/tags/{sha}` — an annotated tag object.
#[derive(Debug, Deserialize)]
pub struct GhTagObject {
    pub object: GhObject,
    pub tagger: Option<GhGitPerson>,
}

/// The `commit` half of a commit: the git object's author and committer.
#[derive(Debug, Deserialize)]
pub struct GhCommitDetail {
    pub committer: Option<GhGitPerson>,
    /// RFC 0019 phase 5 — GitHub's own verdict on the commit's signature.
    #[serde(default)]
    pub verification: Option<GhVerification>,
}

/// `GET /repos/{o}/{r}/branches/{name}`.
#[derive(Debug, Deserialize)]
pub struct GhBranch {
    pub commit: GhBranchCommit,
}

#[derive(Debug, Deserialize)]
pub struct GhBranchCommit {
    pub sha: String,
    pub commit: Option<GhCommitDetail>,
}

/// `GET /repos/{o}/{r}/commits/{sha}`.
#[derive(Debug, Deserialize)]
pub struct GhCommit {
    pub sha: String,
    pub commit: Option<GhCommitDetail>,
    pub committer: Option<GhUser>,
}

/// The `verification` object GitHub puts on a commit (and on a tag object).
/// Confirmed against api.github.com on 2026-09-04 on `cli/cli@trunk`:
/// `{verified, reason, signature, payload}`, with `reason` = `"valid"` on a
/// verified commit.
#[derive(Debug, Deserialize)]
pub(super) struct GhVerification {
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `GET /repos/{o}/{r}/attestations/{sha256:digest}`. Confirmed against
/// api.github.com on 2026-09-04: the endpoint is anonymous-readable and
/// answers `200 {"attestations": []}` when there is none, so an empty array
/// is "no attestation" and a `404` is "this instance has no such endpoint" —
/// which is what GitHub Enterprise below 3.13 answers.
#[derive(Debug, Deserialize)]
pub(super) struct GhAttestations {
    #[serde(default)]
    pub attestations: Vec<serde_json::Value>,
}

/// One entry of `GET /repos/{o}/{r}/tags`. Confirmed against api.github.com
/// on 2026-09-04: `name` and `commit.sha`, with no date on the listing.
#[derive(Debug, Deserialize)]
pub(super) struct GhTag {
    pub name: String,
    pub commit: GhTagCommit,
}

#[derive(Debug, Deserialize)]
pub(super) struct GhTagCommit {
    pub sha: String,
}
