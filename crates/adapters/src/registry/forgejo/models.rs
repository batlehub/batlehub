use serde::Deserialize;

use batlehub_core::error::CoreError;

use super::client::ForgejoRegistryClient;

// ── Serde types for Forgejo/Gitea API responses ──────────────────────────────

#[derive(Debug, Deserialize)]
pub(super) struct FjRelease {
    pub id: u64,
    pub tag_name: String,
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<FjAsset>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjAsset {
    pub id: u64,
    pub name: String,
    pub browser_download_url: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub size: u64,
}

// ── Tags, branches and commits (RFC 0019) ─────────────────────────────────────
//
// Shapes confirmed against codeberg.org on 2026-09-03: `tags/{tag}` carries
// `commit.sha` and `commit.created`; `branches/{name}` carries `commit.id`,
// `commit.timestamp` and `commit.committer.username`; `git/commits/{sha}`
// carries `commit.committer.date` and a top-level `committer.login`.

#[derive(Debug, Deserialize)]
pub(super) struct FjTag {
    pub commit: FjTagCommit,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjTagCommit {
    pub sha: String,
    pub created: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjBranch {
    pub commit: FjBranchCommit,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjBranchCommit {
    pub id: String,
    pub timestamp: Option<String>,
    pub committer: Option<FjPayloadUser>,
}

/// A person as Forgejo's payload objects spell them.
#[derive(Debug, Deserialize)]
pub(super) struct FjPayloadUser {
    pub name: Option<String>,
    pub email: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjGitPerson {
    pub name: Option<String>,
    pub email: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjCommitDetail {
    pub committer: Option<FjGitPerson>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjUser {
    pub login: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FjCommit {
    pub sha: String,
    pub commit: Option<FjCommitDetail>,
    pub committer: Option<FjUser>,
}

impl ForgejoRegistryClient {
    /// Resolve the upstream download URL for a release asset identified either by
    /// filename (`filename/<name>`) or attachment id (numeric). Both look the asset
    /// up inside the release fetched by tag, so the caller must pass a real tag.
    pub(super) async fn asset_download_url(
        &self,
        owner_repo: &str,
        tag: &str,
        artifact: &str,
    ) -> Result<String, CoreError> {
        let release = self.fetch_release_by_tag(owner_repo, tag).await?;
        if let Some(filename) = artifact.strip_prefix("filename/") {
            release
                .assets
                .iter()
                .find(|a| a.name == filename)
                .map(|a| a.browser_download_url.clone())
                .ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "no asset named '{filename}' in {owner_repo}@{tag}"
                    ))
                })
        } else {
            let asset_id: u64 = artifact
                .parse()
                .map_err(|_| CoreError::Registry(format!("invalid asset id: {artifact}")))?;
            release
                .assets
                .iter()
                .find(|a| a.id == asset_id)
                .map(|a| a.browser_download_url.clone())
                .ok_or_else(|| {
                    CoreError::NotFound(format!("asset {asset_id} not found in {owner_repo}@{tag}"))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::registry::forgejo::ForgejoRegistryClient;
    use crate::registry::http_client::UpstreamHttpOptions;
    use batlehub_core::{entities::PackageId, ports::RegistryClient};

    #[tokio::test]
    async fn passthrough_artifacts_resolve_without_http() {
        // raw/tarball/zipball/pkgpath need no release lookup, so resolve_metadata
        // makes no HTTP call — an unreachable upstream proves it never connects.
        let c = ForgejoRegistryClient::new("http://127.0.0.1:1", &UpstreamHttpOptions::default())
            .unwrap();
        for art in [
            "raw/main/README.md",
            "tarball/v1.0.0",
            "zipball",
            "pkgpath/api/packages/owner/generic/x/1/file",
        ] {
            let pkg = PackageId::new("fj", "owner/repo", "v1.0.0").with_artifact(art);
            let md = c
                .resolve_metadata(&pkg)
                .await
                .expect("passthrough must not hit the network");
            assert_eq!(md.id.artifact.as_deref(), Some(art));
            assert!(md.download_url.is_none());
        }
        assert_eq!(c.registry_type(), "forgejo");
    }
}
