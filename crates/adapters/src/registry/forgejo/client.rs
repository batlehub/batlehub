use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;

use std::sync::Arc;

use super::super::forge_api::{parse_date, person_label, BudgetedApi};
use super::super::github::commit_dated_metadata;
use super::super::http_client::{
    apply_upstream_tls, basic_auth_get, ensure_same_origin, fetch_release_listing,
    to_registry_error, upstream_auth_headers, UpstreamHttpOptions,
};
use super::super::ssrf;
use super::models::{FjAsset, FjBranch, FjCommit, FjRelease, FjTag};
use batlehub_core::{
    entities::{is_commit_sha, ForgeProvenance, PackageId, PackageMetadata, RefKind},
    error::CoreError,
    ports::{
        BudgetRole, DocumentKind, FetchedArtifact, ForgeCommit, ForgeRegistry, ForgeTag,
        RateLimitBudget, RegistryClient, ResolvedTarget, VersionDocument,
    },
};

/// Forgejo / Gitea REST API v1 registry client.
///
/// A single adapter serves both Forgejo and Gitea instances — the release API
/// (`/api/v1/repos/{owner}/{repo}/releases`) is identical between them. There is
/// no public default instance, so an upstream URL is required in config; the URL
/// is the instance root (e.g. `https://codeberg.org`), not the API path.
///
/// Supported `PackageId` conventions (mirrors the GitHub adapter):
/// - `version = "releases"` → list releases (metadata only, no artifact)
/// - `version = "v1.0.0"` → release by tag (metadata for age-gate rule)
/// - `artifact = Some("12345678")` → release asset download (by attachment ID)
/// - `artifact = Some("filename/{name}")` → release asset download (by filename)
/// - `artifact = Some("tarball/{ref}")` → source tarball (`/archive/{ref}.tar.gz`)
/// - `artifact = Some("zipball")` → zip archive (`/archive/{ref}.zip`)
/// - `artifact = Some("raw/{path}")` → raw file (`/raw/{ref}/{path}`)
pub struct ForgejoRegistryClient {
    pub(super) http: reqwest::Client,
    /// Credentialed download client (same auth default headers as `http`) with
    /// redirects DISABLED so the SSRF guard validates every hop.
    pub(super) dl_credentialed: reqwest::Client,
    /// Credential-free download client (redirects disabled) used for any redirect
    /// hop that leaves the instance origin, so the token never leaks off-instance.
    pub(super) dl_plain: reqwest::Client,
    /// Instance root, e.g. `https://codeberg.org` (no trailing slash).
    pub(super) base_url: String,
    /// API base, derived as `{base_url}/api/v1`.
    pub(super) api_base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// The rate-limit budget every API call draws on (RFC 0019 §5.2). Forgejo
    /// reports no `X-RateLimit-*` headers, so nothing is observed from it —
    /// the budget still gates the worker's share of the token.
    pub(super) api: BudgetedApi,
    pub(super) token_fingerprint: String,
}

impl ForgejoRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            "application/json"
                .parse()
                .expect("static header value is valid ASCII"),
        );
        let auth_headers = upstream_auth_headers(opts)?;
        headers.extend(auth_headers);

        // `no_redirect` clients follow redirects manually via the SSRF guard, so
        // their own redirect policy is disabled; `with_auth` carries the operator's
        // credentials, which the guard only sends while on the instance origin.
        let build = |no_redirect: bool, with_auth: bool| -> Result<reqwest::Client, CoreError> {
            let mut b = reqwest::Client::builder().user_agent("batlehub/0.1");
            if no_redirect {
                b = b.redirect(reqwest::redirect::Policy::none());
            }
            if with_auth {
                b = b.default_headers(headers.clone());
            }
            apply_upstream_tls(b, opts)
                .map_err(CoreError::Other)?
                .build()
                .map_err(|e| CoreError::Other(e.into()))
        };
        let http = build(false, true)?;
        let dl_credentialed = build(true, true)?;
        let dl_plain = build(true, false)?;

        // The configured URL is the instance root. Strip a trailing `/api/v1`
        // (if a user pasted the API URL) and any trailing slash, then derive the
        // API base from the root.
        let root = base_url.into();
        let root = root.trim_end_matches('/');
        let root = root.trim_end_matches("/api/v1");
        let base_url = root.trim_end_matches('/').to_owned();
        let api_base_url = format!("{base_url}/api/v1");

        let token_fingerprint = batlehub_core::ports::token_fingerprint(
            opts.bearer_token
                .as_deref()
                .or(opts.basic_auth.as_ref().map(|(_, p)| p.as_str()))
                .or(opts.custom_header.as_ref().map(|(_, v)| v.as_str())),
        );

        Ok(Self {
            http,
            dl_credentialed,
            dl_plain,
            base_url,
            api_base_url,
            basic_auth: opts.basic_auth.clone(),
            api: BudgetedApi::unbudgeted(),
            token_fingerprint,
        })
    }

    /// Draw every API call on `budget`, under this registry's name.
    pub fn with_budget(mut self, registry: &str, budget: Arc<dyn RateLimitBudget>) -> Self {
        self.api = BudgetedApi::new(budget, registry, self.token_fingerprint.clone());
        self
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// An API `GET`, through the budget.
    pub(super) async fn api_get(&self, url: &str) -> Result<reqwest::Response, CoreError> {
        self.api.send(self.get(url), BudgetRole::Proxy).await
    }

    /// Fetch every release for `owner_repo`, following `Link: rel="next"`
    /// pagination (Gitea/Forgejo default 50 per page; capped at 20 pages). Returns
    /// `NotFound` if the repository's releases endpoint 404s on the first page.
    pub(super) async fn fetch_all_releases(
        &self,
        owner_repo: &str,
    ) -> Result<Vec<FjRelease>, CoreError> {
        use super::super::http_client::next_link;
        let mut url = format!(
            "{}/repos/{}/releases?limit=50",
            self.api_base_url, owner_repo
        );
        let mut all = Vec::new();
        for page in 0..20 {
            let resp = self.api_get(&url).await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                if page == 0 {
                    return Err(CoreError::NotFound(format!("{owner_repo} not found")));
                }
                break;
            }
            if !resp.status().is_success() {
                return Err(CoreError::Registry(format!(
                    "forgejo: releases list returned {}",
                    resp.status()
                )));
            }
            let next = next_link(resp.headers());
            let releases: Vec<FjRelease> = resp.json().await.map_err(to_registry_error)?;
            all.extend(releases);
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        Ok(all)
    }

    pub(super) async fn fetch_release_by_tag(
        &self,
        owner_repo: &str,
        tag: &str,
    ) -> Result<FjRelease, CoreError> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            self.api_base_url, owner_repo, tag
        );
        let resp = self.api_get(&url).await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo}@{tag} not found")));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<FjRelease>()
            .await
            .map_err(to_registry_error)
    }
}

// ── ForgeRegistry impl (RFC 0019 §6.3) ────────────────────────────────────────

#[async_trait]
impl ForgeRegistry for ForgejoRegistryClient {
    /// Tag first (`tags/{tag}`), then branch (`branches/{name}`), then not
    /// found. Confirmed against codeberg.org on 2026-09-03. Forgejo's tag JSON
    /// dates the *commit* it points at (`commit.created`), not the tag object,
    /// which is RFC 0019 decision 6's answer for a lightweight tag and the best
    /// this API offers for an annotated one.
    async fn resolve_ref(
        &self,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<ResolvedTarget, CoreError> {
        let tag_url = format!(
            "{}/repos/{}/tags/{}",
            self.api_base_url, owner_repo, git_ref
        );
        let resp = self.api_get(&tag_url).await?;
        if resp.status() != reqwest::StatusCode::NOT_FOUND {
            let t: FjTag = resp
                .error_for_status()
                .map_err(to_registry_error)?
                .json()
                .await
                .map_err(to_registry_error)?;
            return Ok(ResolvedTarget {
                kind: RefKind::Tag,
                sha: t.commit.sha,
                object_date: parse_date(t.commit.created.as_deref()),
                publisher: None,
            });
        }

        let branch_url = format!(
            "{}/repos/{}/branches/{}",
            self.api_base_url, owner_repo, git_ref
        );
        let resp = self.api_get(&branch_url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{owner_repo}: no tag or branch named '{git_ref}'"
            )));
        }
        let b: FjBranch = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        let committer = b.commit.committer.as_ref();
        Ok(ResolvedTarget {
            kind: RefKind::Branch,
            sha: b.commit.id,
            object_date: parse_date(b.commit.timestamp.as_deref()),
            publisher: committer.and_then(|c| {
                person_label(c.username.as_deref(), c.name.as_deref(), c.email.as_deref())
            }),
        })
    }

    /// `GET /repos/{o}/{r}/tags` — confirmed against codeberg.org on
    /// 2026-09-04: `name`, `id` (the tag object or the commit) and
    /// `commit.{sha, created}`. Forgejo dates the *commit*, never a tagger,
    /// which is the same limitation the parity table records for its
    /// by-name endpoint.
    async fn tags(&self, owner_repo: &str) -> Result<Vec<ForgeTag>, CoreError> {
        let url = format!("{}/repos/{}/tags?limit=100", self.api_base_url, owner_repo);
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo} not found")));
        }
        let tags: Vec<FjTag> = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(tags
            .into_iter()
            .filter_map(|t| {
                Some(ForgeTag {
                    name: t.name?,
                    sha: t.commit.sha,
                    date: parse_date(t.commit.created.as_deref()),
                })
            })
            .collect())
    }

    /// The commit's own signature, as Forgejo verified it. Forgejo has no
    /// attestation store, so a release asset has no provenance of its own and
    /// the answer is the commit's — never `Unverifiable`, which is GitLab's
    /// alone (RFC 0019 decision 8).
    async fn provenance(
        &self,
        owner_repo: &str,
        sha: &str,
        _asset_digest: Option<&str>,
    ) -> Result<ForgeProvenance, CoreError> {
        let url = format!(
            "{}/repos/{}/git/commits/{}",
            self.api_base_url, owner_repo, sha
        );
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(ForgeProvenance::Missing);
        }
        let c: FjCommit = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        let v = c.commit.as_ref().and_then(|d| d.verification.as_ref());
        Ok(match v {
            Some(v) if v.verified => ForgeProvenance::Verified {
                detail: v.reason.clone().unwrap_or_else(|| "valid".to_owned()),
            },
            Some(v) => {
                let reason = v.reason.clone().unwrap_or_default();
                // `gpg.error.not_signed_commit` is "there is no signature",
                // which is `Missing`; anything else is a signature that
                // failed to verify.
                if reason.is_empty() || reason.contains("not_signed") {
                    ForgeProvenance::Missing
                } else {
                    ForgeProvenance::Invalid { reason }
                }
            }
            None => ForgeProvenance::Missing,
        })
    }

    async fn commit(&self, owner_repo: &str, sha: &str) -> Result<ForgeCommit, CoreError> {
        let url = format!(
            "{}/repos/{}/git/commits/{}",
            self.api_base_url, owner_repo, sha
        );
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{owner_repo}: no commit {sha}"
            )));
        }
        let c: FjCommit = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        let detail = c.commit.as_ref().and_then(|d| d.committer.as_ref());
        Ok(ForgeCommit {
            sha: c.sha,
            committed_at: parse_date(detail.and_then(|d| d.date.as_deref())),
            committer: person_label(
                c.committer.as_ref().and_then(|u| u.login.as_deref()),
                detail.and_then(|d| d.name.as_deref()),
                detail.and_then(|d| d.email.as_deref()),
            ),
        })
    }
}

// ── Pure helper functions (also used by models.rs RegistryClient impl) ────────

pub(super) fn is_release_signed(assets: &[FjAsset]) -> bool {
    assets
        .iter()
        .any(|a| a.name.ends_with(".asc") || a.name.ends_with(".sig"))
}

/// Build a direct download URL for non-API artifact types (tarball, zipball, raw).
///
/// Forgejo/Gitea serve these from the instance root, mirroring GitHub's layout:
/// `{base}/{owner}/{repo}/archive/{ref}.tar.gz` and `{base}/{owner}/{repo}/raw/{ref}/{path}`.
pub(super) fn static_artifact_url(
    artifact: &str,
    base: &str,
    owner_repo: &str,
    git_ref: &str,
) -> Option<String> {
    if artifact.starts_with("tarball/") {
        Some(format!("{base}/{owner_repo}/archive/{git_ref}.tar.gz"))
    } else if artifact == "zipball" {
        Some(format!("{base}/{owner_repo}/archive/{git_ref}.zip"))
    } else {
        artifact
            .strip_prefix("raw/")
            .map(|file_path| format!("{base}/{owner_repo}/raw/{git_ref}/{file_path}"))
    }
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for ForgejoRegistryClient {
    fn registry_type(&self) -> &str {
        "forgejo"
    }

    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        Some(self)
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let url = format!("{}/repos/{}/releases?limit=50", self.base_url, package);
        fetch_release_listing(self.get(&url), kind, "forgejo", "Forgejo", package).await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let owner_repo = &pkg.name;

        // The package-registry passthrough addresses no repository; an archive
        // or raw download names a git ref, which `ProxyService` resolves to a
        // commit before this is called — and the commit is what dates it
        // (RFC 0019 §4.2). An unresolved ref stays undated, as before.
        if let Some(ref artifact) = pkg.artifact {
            if artifact.starts_with("pkgpath/") {
                return Ok(PackageMetadata::minimal(
                    pkg.clone(),
                    serde_json::Value::Null,
                ));
            }
            if artifact.starts_with("raw/")
                || artifact.starts_with("tarball/")
                || artifact == "zipball"
            {
                if !is_commit_sha(&pkg.version) {
                    return Ok(PackageMetadata::minimal(
                        pkg.clone(),
                        serde_json::Value::Null,
                    ));
                }
                return Ok(commit_dated_metadata(
                    pkg,
                    self.commit(owner_repo, &pkg.version).await,
                ));
            }
        }

        match pkg.version.as_str() {
            "releases" => {
                let releases = self.fetch_all_releases(owner_repo).await?;

                let extra = serde_json::to_value(releases.iter().map(|r| {
                    serde_json::json!({ "id": r.id, "tag_name": r.tag_name, "published_at": r.published_at })
                }).collect::<Vec<_>>()).unwrap_or_default();

                Ok(PackageMetadata::minimal(pkg.clone(), extra))
            }

            tag => {
                let release = self.fetch_release_by_tag(owner_repo, tag).await?;

                let published_at = release
                    .published_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));

                let is_signed = is_release_signed(&release.assets);

                let download_url = match &pkg.artifact {
                    Some(artifact_str) => release
                        .assets
                        .iter()
                        .find(|a| {
                            artifact_str
                                .strip_prefix("filename/")
                                .map(|f| a.name == f)
                                .unwrap_or_else(|| artifact_str.parse::<u64>().ok() == Some(a.id))
                        })
                        .map(|a| a.browser_download_url.clone()),
                    None => None,
                };

                let extra = serde_json::json!({
                    "release_id": release.id,
                    "tag_name": release.tag_name,
                    "assets": release.assets.iter().map(|a| serde_json::json!({
                        "id": a.id,
                        "name": a.name,
                        "download_url": a.browser_download_url,
                    })).collect::<Vec<_>>(),
                });

                Ok(PackageMetadata {
                    id: pkg.clone(),
                    published_at,
                    download_url,
                    checksum: None,
                    is_signed: Some(is_signed),
                    extra,
                    cache_control: None,
                })
            }
        }
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let owner_repo = &pkg.name;
        let git_ref = &pkg.version;

        let download_url = match &pkg.artifact {
            // Package-registry passthrough: `pkgpath/<instance-relative-path>` →
            // `{instance}/<path>` (e.g. `api/packages/{owner}/generic/…`).
            Some(artifact) if artifact.starts_with("pkgpath/") => {
                format!("{}/{}", self.base_url, &artifact["pkgpath/".len()..])
            }
            Some(artifact) => {
                if let Some(url) =
                    static_artifact_url(artifact, &self.base_url, owner_repo, git_ref)
                {
                    url
                } else {
                    self.asset_download_url(owner_repo, git_ref, artifact)
                        .await?
                }
            }
            None if git_ref == "releases" => {
                return Err(CoreError::Registry(
                    "the release listing is a document, not an artifact".to_owned(),
                ));
            }
            // `GET /{o}/{r}/releases/tags/{tag}` — the release's own JSON, as the
            // forge sent it (see the GitHub client for why this arm exists).
            None => {
                let url = format!(
                    "{}/repos/{}/releases/tags/{}",
                    self.api_base_url, owner_repo, git_ref
                );
                let resp = self.api_get(&url).await?;
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    return Err(CoreError::NotFound(format!(
                        "{owner_repo}@{git_ref} not found"
                    )));
                }
                let resp = resp.error_for_status().map_err(to_registry_error)?;
                let cache_control = resp
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                return Ok(FetchedArtifact {
                    stream: Box::pin(resp.bytes_stream().map_err(to_registry_error)),
                    cache_control,
                });
            }
        };

        // A release asset's `browser_download_url` comes from the release JSON and
        // Forgejo/Gitea support *external-URL* attachments, so it can point at an
        // arbitrary host. Our client carries the operator's auth as a default
        // header (not stripped cross-host by reqwest), so fetching off-instance
        // would leak that credential and enable SSRF. Require the *initial* URL to
        // share the instance origin (`pkgpath/` and static selectors are built
        // from `base_url` and satisfy this trivially)…
        ensure_same_origin(&download_url, &self.base_url)?;

        tracing::debug!(url = %download_url, "fetching Forgejo artifact");

        // …then follow any redirects manually: each hop is re-validated against
        // private/reserved addresses (SSRF) and credentials are dropped as soon as
        // a redirect leaves the instance origin.
        let parsed = reqwest::Url::parse(&download_url)
            .map_err(|e| CoreError::Registry(format!("invalid download URL: {e}")))?;
        let response = ssrf::fetch_following_redirects(
            &self.dl_credentialed,
            &self.dl_plain,
            &self.basic_auth,
            &self.base_url,
            parsed,
        )
        .await?
        .error_for_status()
        .map_err(to_registry_error)?;

        let cache_control = response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let stream = response.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        match self.fetch_all_releases(package).await {
            Ok(releases) => Ok(releases.into_iter().map(|r| r.tag_name).collect()),
            Err(CoreError::NotFound(_)) => Ok(vec![]),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::ports::RegistryClient;

    fn asset(id: u64, name: &str) -> FjAsset {
        FjAsset {
            id,
            name: name.to_string(),
            browser_download_url: format!("https://example.com/{name}"),
            size: 0,
        }
    }

    #[test]
    fn is_signed_true_when_asc_present() {
        let assets = vec![asset(1, "binary.tar.gz"), asset(2, "binary.tar.gz.asc")];
        assert!(is_release_signed(&assets));
    }

    #[test]
    fn is_signed_false_when_no_sig_asset() {
        let assets = vec![asset(1, "binary.tar.gz"), asset(2, "checksums.txt")];
        assert!(!is_release_signed(&assets));
    }

    #[test]
    fn static_url_tarball() {
        let url = static_artifact_url("tarball/main", "https://codeberg.org", "owner/repo", "v1.0");
        assert_eq!(
            url.as_deref(),
            Some("https://codeberg.org/owner/repo/archive/v1.0.tar.gz")
        );
    }

    #[test]
    fn static_url_raw_file() {
        let url = static_artifact_url(
            "raw/src/main.rs",
            "https://codeberg.org",
            "owner/repo",
            "main",
        );
        assert_eq!(
            url.as_deref(),
            Some("https://codeberg.org/owner/repo/raw/main/src/main.rs")
        );
    }

    #[test]
    fn new_derives_api_base_from_root() {
        let opts = UpstreamHttpOptions::default();
        let client = ForgejoRegistryClient::new("https://codeberg.org/", &opts).unwrap();
        assert_eq!(client.base_url, "https://codeberg.org");
        assert_eq!(client.api_base_url, "https://codeberg.org/api/v1");
    }

    #[test]
    fn new_strips_accidental_api_suffix() {
        let opts = UpstreamHttpOptions::default();
        let client = ForgejoRegistryClient::new("https://git.example.com/api/v1", &opts).unwrap();
        assert_eq!(client.base_url, "https://git.example.com");
        assert_eq!(client.api_base_url, "https://git.example.com/api/v1");
    }

    #[tokio::test]
    async fn list_versions_returns_tags() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::to_string(&serde_json::json!([
            { "id": 1, "tag_name": "v1.1.0", "published_at": "2024-01-02T00:00:00Z", "assets": [] },
            { "id": 2, "tag_name": "v1.0.0", "published_at": "2024-01-01T00:00:00Z", "assets": [] },
        ]))
        .unwrap();
        let _mock = server
            .mock("GET", "/api/v1/repos/owner/repo/releases?limit=50")
            .with_status(200)
            .with_body(&body)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = ForgejoRegistryClient::new(server.url(), &opts).unwrap();
        let versions = client.list_versions("owner/repo").await.unwrap();
        assert_eq!(versions, vec!["v1.1.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn list_versions_follows_pagination() {
        let mut server = mockito::Server::new_async().await;
        let page2_url = format!("{}/api/v1/repos/o/r/releases?limit=50&page=2", server.url());
        let _p1 = server
            .mock("GET", "/api/v1/repos/o/r/releases?limit=50")
            .with_status(200)
            .with_header("link", &format!(r#"<{page2_url}>; rel="next""#))
            .with_body(r#"[{"id":1,"tag_name":"v2.0.0","assets":[]}]"#)
            .create_async()
            .await;
        let _p2 = server
            .mock("GET", "/api/v1/repos/o/r/releases?limit=50&page=2")
            .with_status(200)
            .with_body(r#"[{"id":2,"tag_name":"v1.0.0","assets":[]}]"#)
            .create_async()
            .await;
        let client =
            ForgejoRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let versions = client.list_versions("o/r").await.unwrap();
        assert_eq!(versions, vec!["v2.0.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn fetch_artifact_package_passthrough() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/packages/acme/generic/tool/1.0/tool.bin")
            .with_status(200)
            .with_body(b"BINARY")
            .create_async()
            .await;
        let client =
            ForgejoRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("fj", "_packages", "_")
            .with_artifact("pkgpath/api/packages/acme/generic/tool/1.0/tool.bin");
        let fetched = client.fetch_artifact(&pkg).await.unwrap();
        let body =
            futures::TryStreamExt::try_fold(fetched.stream, Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(body, b"BINARY");
    }

    #[tokio::test]
    async fn list_versions_404_returns_empty() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v1/repos/unknown/repo/releases?limit=50")
            .with_status(404)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = ForgejoRegistryClient::new(server.url(), &opts).unwrap();
        let versions = client.list_versions("unknown/repo").await.unwrap();
        assert!(versions.is_empty());
    }

    #[tokio::test]
    async fn resolve_metadata_releases_list_collects_tags() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::to_string(&serde_json::json!([
            { "id": 2, "tag_name": "v2.0.0", "published_at": null, "assets": [] },
            { "id": 1, "tag_name": "v1.0.0", "published_at": null, "assets": [] },
        ]))
        .unwrap();
        let _m = server
            .mock("GET", "/api/v1/repos/o/r/releases?limit=50")
            .with_status(200)
            .with_body(&body)
            .create_async()
            .await;
        let client =
            ForgejoRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("fj", "o/r", "releases");
        let meta = client.resolve_metadata(&pkg).await.unwrap();
        let tags: Vec<_> = meta
            .extra
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["tag_name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(tags, vec!["v2.0.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn fetch_artifact_static_tarball_streams_bytes() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/o/r/archive/v1.0.0.tar.gz")
            .with_status(200)
            .with_body(b"TARBALL")
            .create_async()
            .await;
        let client =
            ForgejoRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("fj", "o/r", "v1.0.0")
            .with_artifact("tarball/v1.0.0");
        let fetched = client.fetch_artifact(&pkg).await.unwrap();
        let body =
            futures::TryStreamExt::try_fold(fetched.stream, Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(body, b"TARBALL");
    }

    #[tokio::test]
    async fn fetch_artifact_by_filename_resolves_asset() {
        let mut server = mockito::Server::new_async().await;
        let dl = format!("{}/dl/app.bin", server.url());
        let rel = serde_json::to_string(&serde_json::json!({
            "id": 1, "tag_name": "v1", "published_at": null,
            "assets": [ { "id": 9, "name": "app.bin", "browser_download_url": dl, "size": 3 } ]
        }))
        .unwrap();
        let _m1 = server
            .mock("GET", "/api/v1/repos/o/r/releases/tags/v1")
            .with_status(200)
            .with_body(&rel)
            .create_async()
            .await;
        let _m2 = server
            .mock("GET", "/dl/app.bin")
            .with_status(200)
            .with_body(b"BIN")
            .create_async()
            .await;
        let client =
            ForgejoRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("fj", "o/r", "v1")
            .with_artifact("filename/app.bin");
        let fetched = client.fetch_artifact(&pkg).await.unwrap();
        let body =
            futures::TryStreamExt::try_fold(fetched.stream, Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(body, b"BIN");
    }

    #[tokio::test]
    async fn resolve_metadata_by_tag_parses_assets() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::to_string(&serde_json::json!({
            "id": 7,
            "tag_name": "v2.0.0",
            "published_at": "2024-05-01T00:00:00Z",
            "assets": [
                { "id": 11, "name": "app.tar.gz", "browser_download_url": "https://dl/app.tar.gz", "size": 10 },
                { "id": 12, "name": "app.tar.gz.asc", "browser_download_url": "https://dl/app.tar.gz.asc", "size": 1 },
            ]
        }))
        .unwrap();
        let _mock = server
            .mock("GET", "/api/v1/repos/owner/repo/releases/tags/v2.0.0")
            .with_status(200)
            .with_body(&body)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = ForgejoRegistryClient::new(server.url(), &opts).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("fj", "owner/repo", "v2.0.0");
        let meta = client.resolve_metadata(&pkg).await.unwrap();
        assert_eq!(meta.is_signed, Some(true));
        assert!(meta.published_at.is_some());
    }
}

// ── RFC 0019: refs and commits ───────────────────────────────────────────────

#[cfg(test)]
mod forge_tests {
    use super::*;
    use batlehub_core::ports::ForgeRegistry;
    use mockito::Server;

    const COMMIT: &str = "8295dea704e1c18ded9965dc8a20981df30945b9";

    fn client(server: &Server) -> ForgejoRegistryClient {
        ForgejoRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap()
    }

    /// The shape codeberg.org returned for `forgejo/forgejo` `v10.0.0` on
    /// 2026-09-03: the tag names the commit it points at and that commit's date.
    #[tokio::test]
    async fn a_tag_resolves_to_its_commit_and_is_dated_by_it() {
        let mut server = Server::new_async().await;
        let _t = server
            .mock("GET", "/api/v1/repos/forgejo/forgejo/tags/v10.0.0")
            .with_body(format!(
                r#"{{"name":"v10.0.0","id":"39843ee2b33ea9f3c95112cd306462d350b93d32","commit":{{"sha":"{COMMIT}","created":"2025-01-15T22:48:56Z"}}}}"#
            ))
            .create_async()
            .await;
        let t = client(&server)
            .resolve_ref("forgejo/forgejo", "v10.0.0")
            .await
            .unwrap();
        assert_eq!(t.kind, RefKind::Tag);
        assert_eq!(t.sha, COMMIT);
        assert_eq!(
            t.object_date.map(|d| d.to_rfc3339()),
            Some("2025-01-15T22:48:56+00:00".to_owned())
        );
    }

    #[tokio::test]
    async fn a_branch_resolves_after_the_tag_lookup_misses() {
        let mut server = Server::new_async().await;
        let _miss = server
            .mock("GET", "/api/v1/repos/forgejo/forgejo/tags/forgejo")
            .with_status(404)
            .create_async()
            .await;
        let _b = server
            .mock("GET", "/api/v1/repos/forgejo/forgejo/branches/forgejo")
            .with_body(format!(
                r#"{{"name":"forgejo","commit":{{"id":"{COMMIT}","timestamp":"2026-09-03T04:45:22+02:00","committer":{{"name":"Mathieu Fenniak","email":"m@x","username":"mfenniak"}}}}}}"#
            ))
            .create_async()
            .await;
        let t = client(&server)
            .resolve_ref("forgejo/forgejo", "forgejo")
            .await
            .unwrap();
        assert_eq!(t.kind, RefKind::Branch);
        assert_eq!(t.sha, COMMIT);
        assert_eq!(t.publisher.as_deref(), Some("mfenniak"));
        assert_eq!(
            t.object_date.map(|d| d.to_rfc3339()),
            Some("2026-09-03T02:45:22+00:00".to_owned())
        );
    }

    #[tokio::test]
    async fn a_commit_is_dated_by_its_committer() {
        let mut server = Server::new_async().await;
        let _c = server
            .mock("GET", &*format!("/api/v1/repos/o/r/git/commits/{COMMIT}"))
            .with_body(format!(
                r#"{{"sha":"{COMMIT}","created":"2026-09-03T04:45:22+02:00","commit":{{"committer":{{"name":"Mathieu","email":"m@x","date":"2026-09-03T04:45:22+02:00"}}}},"committer":{{"login":"mfenniak"}}}}"#
            ))
            .create_async()
            .await;
        let c = client(&server).commit("o/r", COMMIT).await.unwrap();
        assert_eq!(c.committer.as_deref(), Some("mfenniak"));
        assert!(c.committed_at.is_some());

        let pkg = PackageId::new("fj", "o/r", COMMIT).with_artifact("raw/README.md");
        let meta = client(&server).resolve_metadata(&pkg).await.unwrap();
        assert!(meta.published_at.is_some());
        assert_eq!(meta.extra["forge"]["committer"], "mfenniak");
    }

    #[tokio::test]
    async fn a_release_by_tag_streams_the_forges_own_json() {
        let mut server = Server::new_async().await;
        let body = r#"{"id":1,"tag_name":"v1","assets":[]}"#;
        let _rel = server
            .mock("GET", "/api/v1/repos/o/r/releases/tags/v1")
            .with_body(body)
            .create_async()
            .await;
        let fetched = client(&server)
            .fetch_artifact(&PackageId::new("fj", "o/r", "v1"))
            .await
            .unwrap();
        let got: Vec<u8> = fetched
            .stream
            .try_fold(Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(got, body.as_bytes());
    }

    #[tokio::test]
    async fn an_unknown_ref_is_not_found() {
        let mut server = Server::new_async().await;
        let _t = server
            .mock("GET", "/api/v1/repos/o/r/tags/nope")
            .with_status(404)
            .create_async()
            .await;
        let _b = server
            .mock("GET", "/api/v1/repos/o/r/branches/nope")
            .with_status(404)
            .create_async()
            .await;
        let err = client(&server)
            .resolve_ref("o/r", "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "{err}");
    }
}
