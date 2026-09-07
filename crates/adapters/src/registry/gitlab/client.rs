use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;

use std::sync::Arc;

use super::super::forge_api::{parse_date, person_label, BudgetedApi};
use super::super::http_client::{
    apply_upstream_tls, basic_auth_get, ensure_same_origin, fetch_release_listing, percent_encode,
    to_registry_error, upstream_auth_headers, UpstreamHttpOptions,
};
use super::super::ssrf;
use super::models::{GlBranch, GlCommit, GlLink, GlRelease, GlSignature, GlTag};
use batlehub_core::{
    entities::{ForgeProvenance, RefKind},
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        BudgetRole, DocumentKind, FetchedArtifact, ForgeAsset, ForgeCommit, ForgeRegistry,
        ForgeRelease, ForgeReleaseSource, ForgeTag, RateLimitBudget, RegistryClient,
        ResolvedTarget, VersionDocument,
    },
};

/// GitLab REST API v4 registry client (releases).
///
/// `PackageId` conventions:
/// - `name = "{group}/{subgroup}/{project}"` — the full project path; URL-encoded
///   (`/` → `%2F`) before being used in the API project selector.
/// - `version = "releases"` → list releases (metadata only)
/// - `version = "v1.0.0"` → release by tag
/// - `artifact = Some("link/{name}")` → release link asset (matched by link name)
/// - `artifact = Some("source/{format}")` → source archive via the repository
///   archive endpoint (`format` ∈ `tar.gz`, `zip`, `tar.bz2`, `tar`)
///
/// Auth: GitLab PATs use the `PRIVATE-TOKEN` header — configure it via
/// `upstream_auth` as a custom header. OAuth `Authorization: Bearer` also works.
pub struct GitlabRegistryClient {
    pub(super) http: reqwest::Client,
    /// Credentialed download client (same auth default headers as `http`) with
    /// redirects DISABLED so the SSRF guard validates every hop.
    pub(super) dl_credentialed: reqwest::Client,
    /// Credential-free download client (redirects disabled) used for any redirect
    /// hop that leaves the instance origin, so the token never leaks off-instance.
    pub(super) dl_plain: reqwest::Client,
    /// Instance root, e.g. `https://gitlab.com` (no trailing slash). Used for
    /// package-registry passthrough.
    pub(super) root: String,
    /// API base, derived as `{instance_root}/api/v4`.
    pub(super) api_base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// RFC 0019 §5.2 — every API call draws on the shared budget, as the
    /// GitHub and Forgejo clients have since phase 1. GitLab.com meters by
    /// the minute rather than the hour, and a self-hosted instance meters
    /// whatever its administrator configured; either way the proxy and the
    /// worker share one token and must not spend it twice.
    pub(super) api: BudgetedApi,
    pub(super) token_fingerprint: String,
}

impl GitlabRegistryClient {
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

        // `no_redirect` clients follow redirects manually via the SSRF guard;
        // `with_auth` carries the operator's PAT, which the guard only sends while
        // the request stays on the instance origin.
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

        let root = base_url.into();
        let root = root.trim_end_matches('/');
        let root = root
            .trim_end_matches("/api/v4")
            .trim_end_matches('/')
            .to_owned();
        let api_base_url = format!("{root}/api/v4");

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
            root,
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

    /// An API `GET`, through the budget.
    pub(super) async fn api_get(&self, url: &str) -> Result<reqwest::Response, CoreError> {
        self.api.send(self.get(url), BudgetRole::Proxy).await
    }

    fn repo_url(&self, project: &str, rest: &str) -> String {
        format!(
            "{}/projects/{}/repository/{rest}",
            self.api_base_url,
            Self::project_selector(project)
        )
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// Build the API project selector: the full project path, URL-encoded.
    pub(super) fn project_selector(project: &str) -> String {
        percent_encode(project)
    }

    /// Fetch every release for `project`, following `Link: rel="next"` pagination
    /// (GitLab default 20 per page; we request 100 and cap at 20 pages). Returns
    /// `NotFound` if the project's releases endpoint 404s on the first page.
    pub(super) async fn fetch_all_releases(
        &self,
        project: &str,
    ) -> Result<Vec<GlRelease>, CoreError> {
        use super::super::http_client::next_link;
        let mut url = format!(
            "{}/projects/{}/releases?per_page=100",
            self.api_base_url,
            Self::project_selector(project),
        );
        let mut all = Vec::new();
        for page in 0..20 {
            let resp = self.get(&url).send().await.map_err(to_registry_error)?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                if page == 0 {
                    return Err(CoreError::NotFound(format!("{project} not found")));
                }
                break;
            }
            if !resp.status().is_success() {
                return Err(CoreError::Registry(format!(
                    "gitlab: releases list returned {}",
                    resp.status()
                )));
            }
            let next = next_link(resp.headers());
            let releases: Vec<GlRelease> = resp.json().await.map_err(to_registry_error)?;
            all.extend(releases);
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        Ok(all)
    }

    /// Raw-file download URL via the repository files API:
    /// `{api}/projects/{enc}/repository/files/{enc(path)}/raw?ref={ref}`.
    pub(super) fn raw_file_url(&self, project: &str, git_ref: &str, path: &str) -> String {
        format!(
            "{}/projects/{}/repository/files/{}/raw?ref={}",
            self.api_base_url,
            Self::project_selector(project),
            percent_encode(path),
            percent_encode(git_ref),
        )
    }

    /// Package-registry passthrough URL: `{instance_root}/{relative}` (the relative
    /// path already includes `api/v4/...`).
    pub(super) fn passthrough_url(&self, relative: &str) -> String {
        format!("{}/{}", self.root, relative)
    }

    pub(super) async fn fetch_release_by_tag(
        &self,
        project: &str,
        tag: &str,
    ) -> Result<GlRelease, CoreError> {
        let url = format!(
            "{}/projects/{}/releases/{}",
            self.api_base_url,
            Self::project_selector(project),
            percent_encode(tag),
        );
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{project}@{tag} not found")));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<GlRelease>()
            .await
            .map_err(to_registry_error)
    }

    /// Source-archive download URL via the repository archive endpoint.
    pub(super) fn source_archive_url(&self, project: &str, tag: &str, format: &str) -> String {
        format!(
            "{}/projects/{}/repository/archive.{}?sha={}",
            self.api_base_url,
            Self::project_selector(project),
            format,
            percent_encode(tag),
        )
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

pub(super) fn is_release_signed(links: &[GlLink]) -> bool {
    links
        .iter()
        .any(|l| l.name.ends_with(".asc") || l.name.ends_with(".sig"))
}

/// If `artifact` selects a source archive (`source/{format}`), return the format.
pub(super) fn source_format(artifact: &str) -> Option<&str> {
    artifact.strip_prefix("source/")
}

// ── ForgeReleaseSource impl (RFC 0021 §5.2) ───────────────────────────────────

/// GitLab attaches *links*, not files, and addresses them `link/<name>` — the
/// same sub-coordinate `resolve_metadata` reads back, so an import fetches
/// through `RegistryClient::fetch_artifact` and the link's own host still goes
/// through this registry's SSRF guard.
///
/// `sources` are deliberately not offered: they are the repository tarball the
/// forge generates, not something a team released.
fn gl_release(release: GlRelease) -> ForgeRelease {
    ForgeRelease {
        tag: release.tag_name,
        // GitLab has no drafts.
        draft: false,
        prerelease: release.upcoming_release,
        assets: release
            .assets
            .links
            .into_iter()
            .map(|l| ForgeAsset {
                artifact: format!("link/{}", l.name),
                name: l.name,
                size: None,
            })
            .collect(),
    }
}

#[async_trait]
impl ForgeReleaseSource for GitlabRegistryClient {
    async fn list_releases(&self, repo: &str) -> Result<Vec<ForgeRelease>, CoreError> {
        match self.fetch_all_releases(repo).await {
            Ok(releases) => Ok(releases.into_iter().map(gl_release).collect()),
            Err(CoreError::NotFound(_)) => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    async fn release_by_tag(&self, repo: &str, tag: &str) -> Result<ForgeRelease, CoreError> {
        self.fetch_release_by_tag(repo, tag).await.map(gl_release)
    }
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

/// RFC 0019 phase 4 — GitLab at the parity the other two forges reached in
/// phase 1. Every endpoint below was confirmed against gitlab.com on
/// 2026-09-04 (`gitlab-org/cli`); see `models.rs` for the shapes.
#[async_trait]
impl ForgeRegistry for GitlabRegistryClient {
    /// Tag first (`/repository/tags/{tag}`), then branch
    /// (`/repository/branches/{name}`), then not found — the order §4.2
    /// documents and the resolver's tests pin.
    ///
    /// GitLab returns the tag's *commit* inline, so an annotated tag costs
    /// one call rather than GitHub's two: `commit.id` is already the commit
    /// the tag points at, and `created_at` is the tag's own date when it has
    /// one.
    async fn resolve_ref(
        &self,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<ResolvedTarget, CoreError> {
        let tag_url = self.repo_url(owner_repo, &format!("tags/{}", percent_encode(git_ref)));
        let resp = self.api_get(&tag_url).await?;
        if resp.status() != reqwest::StatusCode::NOT_FOUND {
            let t: GlTag = resp
                .error_for_status()
                .map_err(to_registry_error)?
                .json()
                .await
                .map_err(to_registry_error)?;
            return Ok(ResolvedTarget {
                kind: RefKind::Tag,
                sha: t.commit.id,
                // The tag's own date for an annotated tag; the commit's
                // otherwise — RFC 0019 decision 6, so a resolvable tag is
                // never `TIMESTAMP_MISSING`.
                object_date: parse_date(t.created_at.as_deref())
                    .or_else(|| parse_date(t.commit.committed_date.as_deref())),
                publisher: person_label(
                    None,
                    t.commit.committer_name.as_deref(),
                    t.commit.committer_email.as_deref(),
                ),
            });
        }

        let branch_url =
            self.repo_url(owner_repo, &format!("branches/{}", percent_encode(git_ref)));
        let resp = self.api_get(&branch_url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{owner_repo}: no tag or branch named '{git_ref}'"
            )));
        }
        let b: GlBranch = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(ResolvedTarget {
            kind: RefKind::Branch,
            sha: b.commit.id,
            object_date: parse_date(b.commit.committed_date.as_deref()),
            publisher: person_label(
                None,
                b.commit.committer_name.as_deref(),
                b.commit.committer_email.as_deref(),
            ),
        })
    }

    async fn commit(&self, owner_repo: &str, sha: &str) -> Result<ForgeCommit, CoreError> {
        let url = self.repo_url(owner_repo, &format!("commits/{}", percent_encode(sha)));
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{owner_repo}: no commit {sha}"
            )));
        }
        let c: GlCommit = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(ForgeCommit {
            sha: c.id,
            committed_at: parse_date(c.committed_date.as_deref()),
            committer: person_label(
                None,
                c.committer_name.as_deref(),
                c.committer_email.as_deref(),
            ),
        })
    }

    /// The commit's signature; failing that, the release evidence GitLab
    /// collects — which exists but cannot be cryptographically verified, and
    /// is the **one** source in this codebase that reports `Unverifiable`
    /// (RFC 0019 decision 8).
    async fn provenance(
        &self,
        owner_repo: &str,
        sha: &str,
        _asset_digest: Option<&str>,
    ) -> Result<ForgeProvenance, CoreError> {
        if let Some(sig) = self.commit_signature(owner_repo, sha).await? {
            let status = sig.verification_status.unwrap_or_default();
            let kind = sig.signature_type.unwrap_or_else(|| "signature".to_owned());
            return Ok(if status == "verified" {
                ForgeProvenance::Verified {
                    detail: format!("{kind} signature verified by GitLab"),
                }
            } else {
                ForgeProvenance::Invalid {
                    reason: format!("{kind} signature is '{status}'"),
                }
            });
        }
        // No signature. A *release* coordinate may still have GitLab's own
        // evidence, which exists and cannot be checked — the one place
        // `Unverifiable` comes from (decision 8). `sha` is the tag for a
        // release or asset coordinate; for a commit the lookup 404s and the
        // answer stays `Missing`, which is the honest reading.
        if let Ok(release) = self.fetch_release_by_tag(owner_repo, sha).await {
            if !release.evidences.is_empty() {
                return Ok(ForgeProvenance::Unverifiable {
                    detail: format!(
                        "GitLab collected {} release evidence blob(s) for {sha}; they are not \
                         cryptographically verifiable",
                        release.evidences.len()
                    ),
                });
            }
        }
        Ok(ForgeProvenance::Missing)
    }

    async fn tags(&self, owner_repo: &str) -> Result<Vec<ForgeTag>, CoreError> {
        let url = self.repo_url(owner_repo, "tags?per_page=100");
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo} not found")));
        }
        let tags: Vec<GlTag> = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(tags
            .into_iter()
            .map(|t| ForgeTag {
                date: parse_date(t.created_at.as_deref())
                    .or_else(|| parse_date(t.commit.committed_date.as_deref())),
                name: t.name,
                sha: t.commit.id,
            })
            .collect())
    }
}

impl GitlabRegistryClient {
    /// The commit's signature, or `None` when GitLab says there is none.
    ///
    /// `404` is the answer for an unsigned commit — confirmed 2026-09-04,
    /// body `{"message":"404 Signature Not Found"}` — so it is mapped to
    /// `None` rather than propagated: "this commit is not signed" is a fact,
    /// not a failure.
    pub(super) async fn commit_signature(
        &self,
        project: &str,
        sha: &str,
    ) -> Result<Option<GlSignature>, CoreError> {
        let url = self.repo_url(
            project,
            &format!("commits/{}/signature", percent_encode(sha)),
        );
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(
            resp.error_for_status()
                .map_err(to_registry_error)?
                .json()
                .await
                .map_err(to_registry_error)?,
        ))
    }
}

#[async_trait]
impl RegistryClient for GitlabRegistryClient {
    fn releases(&self) -> Option<&dyn batlehub_core::ports::ForgeReleaseSource> {
        Some(self)
    }

    fn registry_type(&self) -> &str {
        "gitlab"
    }

    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        Some(self)
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let url = format!(
            "{}/projects/{}/releases?per_page=100",
            self.api_base_url,
            percent_encode(package)
        );
        fetch_release_listing(self.get(&url), kind, "gitlab", "GitLab", package).await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let project = &pkg.name;

        // Source archives, raw files, and package-registry passthrough need no
        // release lookup — return minimal metadata.
        if let Some(ref artifact) = pkg.artifact {
            if source_format(artifact).is_some()
                || artifact.starts_with("rawfile/")
                || artifact.starts_with("pkgpath/")
            {
                return Ok(PackageMetadata::minimal(
                    pkg.clone(),
                    serde_json::Value::Null,
                ));
            }
        }

        match pkg.version.as_str() {
            "releases" => {
                let releases = self.fetch_all_releases(project).await?;

                let extra = serde_json::to_value(
                    releases
                        .iter()
                        .map(|r| {
                            serde_json::json!({ "tag_name": r.tag_name, "released_at": r.released_at })
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default();

                Ok(PackageMetadata::minimal(pkg.clone(), extra))
            }

            tag => {
                let release = self.fetch_release_by_tag(project, tag).await?;

                let published_at = release
                    .released_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));

                let is_signed = is_release_signed(&release.assets.links);

                let download_url = match &pkg.artifact {
                    Some(artifact) => artifact.strip_prefix("link/").and_then(|name| {
                        release
                            .assets
                            .links
                            .iter()
                            .find(|l| l.name == name)
                            .map(|l| l.direct_asset_url.clone().unwrap_or_else(|| l.url.clone()))
                    }),
                    None => None,
                };

                let extra = serde_json::json!({
                    "tag_name": release.tag_name,
                    "links": release.assets.links.iter().map(|l| serde_json::json!({
                        "name": l.name,
                        "url": l.url,
                    })).collect::<Vec<_>>(),
                    "sources": release.assets.sources.iter().map(|s| serde_json::json!({
                        "format": s.format,
                        "url": s.url,
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
        let project = &pkg.name;
        let tag = &pkg.version;

        let download_url = match &pkg.artifact {
            Some(artifact) => {
                if let Some(format) = source_format(artifact) {
                    self.source_archive_url(project, tag, format)
                } else if let Some(name) = artifact.strip_prefix("link/") {
                    self.link_download_url(project, tag, name).await?
                } else if let Some(rest) = artifact.strip_prefix("rawfile/") {
                    // `rawfile/{ref}/{path}` → repository raw-file API.
                    let (git_ref, path) = rest.split_once('/').ok_or_else(|| {
                        CoreError::Registry(format!("invalid rawfile selector: {artifact}"))
                    })?;
                    self.raw_file_url(project, git_ref, path)
                } else if let Some(rest) = artifact.strip_prefix("pkgpath/") {
                    // Package-registry passthrough (`api/v4/projects/.../packages/…`).
                    self.passthrough_url(rest)
                } else {
                    return Err(CoreError::Registry(format!(
                        "unsupported gitlab artifact selector: {artifact}"
                    )));
                }
            }
            None => {
                return Err(CoreError::Registry(
                    "fetch_artifact requires PackageId::artifact to be set".to_owned(),
                ));
            }
        };

        // A release "link" asset URL comes straight from the release JSON
        // (`direct_asset_url`/`url`), i.e. it is set by the project owner and can
        // point anywhere. Our client carries the operator's `PRIVATE-TOKEN` as a
        // default header (which reqwest does NOT strip on cross-host requests),
        // so fetching an off-instance URL would exfiltrate that credential and
        // enable SSRF (e.g. `http://169.254.169.254/…`). Require the resolved URL
        // to share the instance origin before fetching — the same guard npm and
        // OpenVSX already apply to upstream-supplied download URLs. The other
        // selectors (source archive, raw file, package passthrough) are built
        // from `api_base_url`, so they satisfy this trivially.
        ensure_same_origin(&download_url, &self.root)?;

        tracing::debug!(url = %download_url, "fetching GitLab artifact");

        // Follow redirects manually so each hop is re-validated against
        // private/reserved addresses (SSRF) and the PAT is dropped the moment a
        // redirect leaves the instance origin.
        let parsed = reqwest::Url::parse(&download_url)
            .map_err(|e| CoreError::Registry(format!("invalid download URL: {e}")))?;
        let response = ssrf::fetch_following_redirects(
            &self.dl_credentialed,
            &self.dl_plain,
            &self.basic_auth,
            &self.root,
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

    #[test]
    fn project_selector_encodes_slashes() {
        assert_eq!(
            GitlabRegistryClient::project_selector("group/sub/proj"),
            "group%2Fsub%2Fproj"
        );
    }

    #[test]
    fn new_derives_api_base() {
        let opts = UpstreamHttpOptions::default();
        let client = GitlabRegistryClient::new("https://gitlab.com/", &opts).unwrap();
        assert_eq!(client.api_base_url, "https://gitlab.com/api/v4");
    }

    // ── RFC 0019 phase 4: the ref surface, as gitlab.com answers it ──────
    //
    // The bodies below are trimmed copies of what gitlab.com returned for
    // `gitlab-org/cli` on 2026-09-04, which is what makes these regression
    // tests rather than assertions about a shape we invented.

    async fn forge_client(server: &mockito::Server) -> GitlabRegistryClient {
        GitlabRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap()
    }

    const TAG_BODY: &str = r#"{
        "name": "v1.40.0",
        "message": "",
        "target": "9ed43f6507f1214b165fcb79f88e5e503f4a3539",
        "created_at": "2024-04-25T09:00:00.000+00:00",
        "commit": {
            "id": "9ed43f6507f1214b165fcb79f88e5e503f4a3539",
            "committed_date": "2024-04-24T19:27:12.000+00:00",
            "committer_name": "Oscar Tovar",
            "committer_email": "otovar@gitlab.com"
        }
    }"#;

    #[tokio::test]
    async fn a_tag_resolves_in_one_call_and_prefers_its_own_date() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/repository/tags/v1.40.0")
            .with_status(200)
            .with_body(TAG_BODY)
            .create_async()
            .await;
        let c = forge_client(&server).await;
        let r = c.resolve_ref("grp/proj", "v1.40.0").await.unwrap();
        assert_eq!(r.kind, RefKind::Tag);
        assert_eq!(r.sha, "9ed43f6507f1214b165fcb79f88e5e503f4a3539");
        // The tag's own date, not the commit's — decision 6.
        assert_eq!(
            r.object_date.map(|d| d.to_rfc3339()),
            Some("2024-04-25T09:00:00+00:00".to_owned())
        );
        assert!(r.publisher.as_deref().unwrap().contains("Oscar Tovar"));
    }

    #[tokio::test]
    async fn a_branch_is_asked_for_only_after_the_tag_is_not_found() {
        let mut server = mockito::Server::new_async().await;
        let tag = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/repository/tags/main")
            .with_status(404)
            .with_body(r#"{"message":"404 Tag Not Found"}"#)
            .create_async()
            .await;
        let _branch = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/branches/main",
            )
            .with_status(200)
            .with_body(
                r#"{"name":"main","commit":{"id":"a2f593651954e9023db48f52192e3ed6be600f60",
                    "committed_date":"2026-09-04T12:01:47.000+02:00",
                    "committer_name":"GitLab","committer_email":"noreply@gitlab.com"}}"#,
            )
            .create_async()
            .await;
        let c = forge_client(&server).await;
        let r = c.resolve_ref("grp/proj", "main").await.unwrap();
        assert_eq!(r.kind, RefKind::Branch);
        assert_eq!(r.sha, "a2f593651954e9023db48f52192e3ed6be600f60");
        assert!(r.object_date.is_some());
        tag.assert_async().await;
    }

    #[tokio::test]
    async fn a_ref_that_is_neither_is_not_found() {
        let mut server = mockito::Server::new_async().await;
        let _t = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/repository/tags/nope")
            .with_status(404)
            .create_async()
            .await;
        let _b = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/branches/nope",
            )
            .with_status(404)
            .create_async()
            .await;
        let c = forge_client(&server).await;
        assert!(matches!(
            c.resolve_ref("grp/proj", "nope").await,
            Err(CoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn a_commit_is_flat_and_a_tag_list_carries_its_dates() {
        let mut server = mockito::Server::new_async().await;
        let _c = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/commits/9ed43f6507f1214b165fcb79f88e5e503f4a3539",
            )
            .with_status(200)
            .with_body(
                r#"{"id":"9ed43f6507f1214b165fcb79f88e5e503f4a3539",
                    "committed_date":"2024-04-24T19:27:12.000+00:00",
                    "committer_name":"Oscar Tovar","committer_email":"otovar@gitlab.com"}"#,
            )
            .create_async()
            .await;
        let _t = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/tags?per_page=100",
            )
            .with_status(200)
            .with_body(format!("[{TAG_BODY}]"))
            .create_async()
            .await;
        let c = forge_client(&server).await;
        let commit = c
            .commit("grp/proj", "9ed43f6507f1214b165fcb79f88e5e503f4a3539")
            .await
            .unwrap();
        assert_eq!(commit.sha, "9ed43f6507f1214b165fcb79f88e5e503f4a3539");
        assert!(commit.committed_at.is_some());

        let tags = c.tags("grp/proj").await.unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "v1.40.0");
        assert!(tags[0].date.is_some(), "GitLab dates its tag list");
    }

    /// GitLab answers `404` for an unsigned commit, which is a fact about the
    /// commit rather than a failure of the request.
    #[tokio::test]
    async fn an_unsigned_commit_has_no_signature_rather_than_an_error() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/commits/abc/signature",
            )
            .with_status(404)
            .with_body(r#"{"message":"404 Signature Not Found"}"#)
            .create_async()
            .await;
        let c = forge_client(&server).await;
        assert!(c
            .commit_signature("grp/proj", "abc")
            .await
            .unwrap()
            .is_none());
    }

    /// RFC 0019 decision 8: GitLab's release evidence is the one source of
    /// `PROVENANCE_UNVERIFIABLE` in this codebase.
    #[tokio::test]
    async fn release_evidence_is_unverifiable_and_nothing_else_is() {
        let mut server = mockito::Server::new_async().await;
        let _sig = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/commits/v1.40.0/signature",
            )
            .with_status(404)
            .create_async()
            .await;
        let _rel = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases/v1.40.0")
            .with_status(200)
            .with_body(
                r#"{"tag_name":"v1.40.0","released_at":"2024-04-25T00:00:00Z",
                    "evidences":[{"sha":"773f18b1","filepath":"https://gitlab.com/x.json",
                    "collected_at":"2024-04-25T02:55:30.443Z"}],
                    "assets":{"links":[],"sources":[]}}"#,
            )
            .create_async()
            .await;
        let c = forge_client(&server).await;
        let p = c.provenance("grp/proj", "v1.40.0", None).await.unwrap();
        assert!(
            matches!(
                p,
                batlehub_core::entities::ForgeProvenance::Unverifiable { .. }
            ),
            "{p:?}"
        );
    }

    #[tokio::test]
    async fn a_verified_signature_beats_the_evidence_and_no_release_is_missing() {
        let mut server = mockito::Server::new_async().await;
        let _sig = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/commits/abc/signature",
            )
            .with_status(200)
            .with_body(r#"{"signature_type":"PGP","verification_status":"verified"}"#)
            .create_async()
            .await;
        let c = forge_client(&server).await;
        assert!(matches!(
            c.provenance("grp/proj", "abc", None).await.unwrap(),
            batlehub_core::entities::ForgeProvenance::Verified { .. }
        ));

        let mut server = mockito::Server::new_async().await;
        let _sig = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/commits/abc/signature",
            )
            .with_status(404)
            .create_async()
            .await;
        let _rel = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases/abc")
            .with_status(404)
            .create_async()
            .await;
        let c = forge_client(&server).await;
        assert_eq!(
            c.provenance("grp/proj", "abc", None).await.unwrap(),
            batlehub_core::entities::ForgeProvenance::Missing
        );
    }

    #[test]
    fn source_format_extracts() {
        assert_eq!(source_format("source/tar.gz"), Some("tar.gz"));
        assert_eq!(source_format("link/foo"), None);
    }

    #[test]
    fn raw_file_and_passthrough_urls() {
        let opts = UpstreamHttpOptions::default();
        let client = GitlabRegistryClient::new("https://gitlab.com", &opts).unwrap();
        assert_eq!(
            client.raw_file_url("grp/proj", "main", "src/x.rs"),
            "https://gitlab.com/api/v4/projects/grp%2Fproj/repository/files/src%2Fx.rs/raw?ref=main"
        );
        assert_eq!(
            client.passthrough_url("api/v4/projects/1/packages/generic/a/1.0/f.bin"),
            "https://gitlab.com/api/v4/projects/1/packages/generic/a/1.0/f.bin"
        );
    }

    #[tokio::test]
    async fn list_versions_follows_pagination() {
        let mut server = mockito::Server::new_async().await;
        let p2 = format!(
            "{}/api/v4/projects/grp%2Fproj/releases?per_page=100&page=2",
            server.url()
        );
        let _m1 = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases?per_page=100")
            .with_status(200)
            .with_header("link", &format!(r#"<{p2}>; rel="next""#))
            .with_body(r#"[{"tag_name":"v2.0.0","assets":{"links":[],"sources":[]}}]"#)
            .create_async()
            .await;
        let _m2 = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/releases?per_page=100&page=2",
            )
            .with_status(200)
            .with_body(r#"[{"tag_name":"v1.0.0","assets":{"links":[],"sources":[]}}]"#)
            .create_async()
            .await;
        let client =
            GitlabRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let versions = client.list_versions("grp/proj").await.unwrap();
        assert_eq!(versions, vec!["v2.0.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn fetch_artifact_rawfile_and_package_passthrough() {
        let mut server = mockito::Server::new_async().await;
        let _raw = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/files/README.md/raw?ref=main",
            )
            .with_status(200)
            .with_body(b"# readme")
            .create_async()
            .await;
        let _pkg = server
            .mock("GET", "/api/v4/projects/1/packages/generic/a/1.0/f.bin")
            .with_status(200)
            .with_body(b"PKG")
            .create_async()
            .await;
        let client =
            GitlabRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();

        let raw = batlehub_core::entities::PackageId::new("gl", "grp/proj", "main")
            .with_artifact("rawfile/main/README.md");
        let body = collect(client.fetch_artifact(&raw).await.unwrap()).await;
        assert_eq!(body, b"# readme");

        let pkg = batlehub_core::entities::PackageId::new("gl", "_packages", "_")
            .with_artifact("pkgpath/api/v4/projects/1/packages/generic/a/1.0/f.bin");
        let body = collect(client.fetch_artifact(&pkg).await.unwrap()).await;
        assert_eq!(body, b"PKG");
    }

    async fn collect(f: batlehub_core::ports::FetchedArtifact) -> Vec<u8> {
        futures::TryStreamExt::try_fold(f.stream, Vec::new(), |mut a, c| async move {
            a.extend_from_slice(&c);
            Ok(a)
        })
        .await
        .unwrap()
    }

    /// GitLab attaches *links*, not files, and has no drafts — its own name for
    /// "not the default download" is `upcoming_release` (RFC 0021 §11 q5).
    #[tokio::test]
    async fn releases_are_normalised_from_links_and_upcoming_release() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases?per_page=100")
            .with_body(
                r#"[
                  {"tag_name":"v2","upcoming_release":true,
                   "assets":{"links":[],"sources":[]}},
                  {"tag_name":"v1",
                   "assets":{"links":[{"name":"ext-1.0.0.vsix",
                                       "url":"https://example.invalid/a.vsix"}],
                             "sources":[]}}
                ]"#,
            )
            .create_async()
            .await;
        let client =
            GitlabRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();

        let got = client.list_releases("grp/proj").await.unwrap();

        assert_eq!(got.len(), 2);
        assert!(
            got[0].prerelease,
            "upcoming_release is GitLab's pre-release"
        );
        assert!(!got[0].draft, "GitLab has no drafts");
        assert!(got[1].is_stable());
        assert_eq!(got[1].assets[0].artifact, "link/ext-1.0.0.vsix");
        assert_eq!(got[1].assets[0].size, None, "a link reports no size");
    }

    #[tokio::test]
    async fn list_versions_returns_tags() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::to_string(&serde_json::json!([
            { "tag_name": "v1.1.0", "released_at": "2024-01-02T00:00:00Z", "assets": { "links": [], "sources": [] } },
            { "tag_name": "v1.0.0", "released_at": "2024-01-01T00:00:00Z", "assets": { "links": [], "sources": [] } },
        ]))
        .unwrap();
        let _mock = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases?per_page=100")
            .with_status(200)
            .with_body(&body)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = GitlabRegistryClient::new(server.url(), &opts).unwrap();
        let versions = client.list_versions("grp/proj").await.unwrap();
        assert_eq!(versions, vec!["v1.1.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn fetch_artifact_source_archive_streams() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock(
                "GET",
                "/api/v4/projects/grp%2Fproj/repository/archive.tar.gz?sha=v1.0.0",
            )
            .with_status(200)
            .with_body(b"SRC")
            .create_async()
            .await;
        let client =
            GitlabRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("gl", "grp/proj", "v1.0.0")
            .with_artifact("source/tar.gz");
        let fetched = client.fetch_artifact(&pkg).await.unwrap();
        let body =
            futures::TryStreamExt::try_fold(fetched.stream, Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(body, b"SRC");
    }

    #[tokio::test]
    async fn fetch_artifact_link_resolves_via_release() {
        let mut server = mockito::Server::new_async().await;
        let dl = format!("{}/d/app.bin", server.url());
        let rel = serde_json::to_string(&serde_json::json!({
            "tag_name": "v1.0.0", "released_at": null,
            "assets": { "links": [ { "name": "app.bin", "url": dl } ], "sources": [] }
        }))
        .unwrap();
        let _m1 = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases/v1.0.0")
            .with_status(200)
            .with_body(&rel)
            .create_async()
            .await;
        let _m2 = server
            .mock("GET", "/d/app.bin")
            .with_status(200)
            .with_body(b"BIN")
            .create_async()
            .await;
        let client =
            GitlabRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("gl", "grp/proj", "v1.0.0")
            .with_artifact("link/app.bin");
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
    async fn resolve_metadata_by_tag_detects_signature_link() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::to_string(&serde_json::json!({
            "tag_name": "v2.0.0",
            "released_at": "2024-05-01T00:00:00Z",
            "assets": {
                "links": [
                    { "name": "app.bin", "url": "https://dl/app.bin", "direct_asset_url": "https://dl/d/app.bin" },
                    { "name": "app.bin.asc", "url": "https://dl/app.bin.asc" },
                ],
                "sources": [ { "format": "tar.gz", "url": "https://dl/src.tar.gz" } ]
            }
        }))
        .unwrap();
        let _mock = server
            .mock("GET", "/api/v4/projects/grp%2Fproj/releases/v2.0.0")
            .with_status(200)
            .with_body(&body)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = GitlabRegistryClient::new(server.url(), &opts).unwrap();
        let pkg = batlehub_core::entities::PackageId::new("gl", "grp/proj", "v2.0.0");
        let meta = client.resolve_metadata(&pkg).await.unwrap();
        assert_eq!(meta.is_signed, Some(true));
        assert!(meta.published_at.is_some());
    }
}
