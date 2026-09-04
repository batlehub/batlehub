use serde::Deserialize;

// ── Serde types for GitHub API responses ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(super) struct GhRelease {
    pub id: u64,
    pub tag_name: String,
    pub published_at: Option<String>,
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GhAsset {
    pub id: u64,
    pub name: String,
    pub browser_download_url: String,
    #[allow(dead_code)]
    pub size: u64,
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
