use serde::Deserialize;

use batlehub_core::error::CoreError;

use super::client::GitlabRegistryClient;

// ── Serde types for GitLab API responses ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(super) struct GlRelease {
    pub tag_name: String,
    /// RFC 0019 decision 8 — GitLab collects a JSON "evidence" blob per
    /// release and signs nothing. Confirmed against gitlab.com on
    /// 2026-09-04: `[{sha, filepath, collected_at}]`. It exists, it cannot be
    /// cryptographically verified, and it is the **only** source in this
    /// codebase that reports `Unverifiable`.
    #[serde(default)]
    pub evidences: Vec<serde_json::Value>,
    /// GitLab uses `released_at` (not `published_at`).
    pub released_at: Option<String>,
    #[serde(default)]
    pub assets: GlAssets,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct GlAssets {
    #[serde(default)]
    pub links: Vec<GlLink>,
    #[serde(default)]
    pub sources: Vec<GlSource>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GlLink {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub direct_asset_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GlSource {
    pub format: String,
    pub url: String,
}

// ── Refs, tags, commits and branches (RFC 0019 phase 4) ───────────────────────
//
// Shapes confirmed against gitlab.com on 2026-09-04, on `gitlab-org/cli`:
//
// * `/repository/tags/{tag}` → `{name, message, target, created_at,
//   commit:{id, committed_date, committer_name, committer_email, ...}}`.
//   `target` is the tag object for an annotated tag and the commit for a
//   lightweight one; `created_at` is the tag's own date when annotated.
// * `/repository/commits/{sha}` → `{id, committed_date, committer_name,
//   committer_email, ...}` — flat, unlike GitHub's nested `commit.committer`.
// * `/repository/branches/{name}` → `{name, commit:{id, committed_date,
//   committer_name, committer_email}}`.
// * `/repository/commits/{sha}/signature` → `404 {"message":"404 Signature
//   Not Found"}` on an unsigned commit; the endpoint exists, and its absence
//   is the answer rather than an error.

/// One commit as GitLab's repository API returns it — flat, with the person
/// as two strings rather than an object.
#[derive(Debug, Deserialize)]
pub(super) struct GlCommit {
    pub id: String,
    #[serde(default)]
    pub committed_date: Option<String>,
    #[serde(default)]
    pub committer_name: Option<String>,
    #[serde(default)]
    pub committer_email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GlTag {
    pub name: String,
    /// The tag's own creation date; present for an annotated tag.
    #[serde(default)]
    pub created_at: Option<String>,
    pub commit: GlCommit,
}

#[derive(Debug, Deserialize)]
pub(super) struct GlBranch {
    pub commit: GlCommit,
}

/// `/repository/commits/{sha}/signature`, when there is one. GitLab answers
/// `404` when the commit is unsigned, so this type is only ever built from a
/// `200`.
#[derive(Debug, Deserialize)]
pub(super) struct GlSignature {
    /// `PGP`, `X509`, `SSH`.
    #[serde(default)]
    pub signature_type: Option<String>,
    /// `verified`, `unverified`, `unknown_key`, `unverified_key`, …
    #[serde(default)]
    pub verification_status: Option<String>,
}

impl GitlabRegistryClient {
    /// Resolve a release link asset to its upstream download URL, matched by the
    /// link `name`.
    pub(super) async fn link_download_url(
        &self,
        project: &str,
        tag: &str,
        name: &str,
    ) -> Result<String, CoreError> {
        let release = self.fetch_release_by_tag(project, tag).await?;
        release
            .assets
            .links
            .iter()
            .find(|l| l.name == name)
            // Prefer the direct asset URL (stable permalink) when present.
            .map(|l| l.direct_asset_url.clone().unwrap_or_else(|| l.url.clone()))
            .ok_or_else(|| {
                CoreError::NotFound(format!("no release link named '{name}' in {project}@{tag}"))
            })
    }
}
