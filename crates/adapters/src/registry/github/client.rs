use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;

use std::sync::Arc;

use super::super::forge_api::{parse_date, person_label, BudgetedApi};
use super::super::http_client::{
    apply_upstream_tls, basic_auth_get, ensure_same_origin, fetch_release_listing,
    to_registry_error, upstream_auth_headers, UpstreamHttpOptions,
};
use super::super::ssrf;
use super::models::{
    GhAsset, GhAttestations, GhBranch, GhCommit, GhRef, GhRelease, GhTag, GhTagObject,
    GhVerification,
};
use batlehub_core::{
    entities::{
        is_commit_sha, ForgeProvenance, PackageId, PackageMetadata, RefKind, FORGE_ASSET_DIGEST,
        FORGE_EXTRA_KEY, UNKNOWN_TAG,
    },
    error::CoreError,
    ports::{
        BudgetRole, DocumentKind, FetchedArtifact, ForgeAsset, ForgeCommit, ForgeRegistry,
        ForgeRelease, ForgeReleaseSource, ForgeTag, RateLimitBudget, RegistryClient,
        ResolvedTarget, VersionDocument,
    },
};

/// GitHub REST API v3 registry client.
///
/// Supported `PackageId` conventions:
/// - `version = "releases"` → list releases (metadata only, no artifact)
/// - `version = "v1.80.0"` → release by tag (metadata for age-gate rule)
/// - `artifact = Some("12345678")` → specific release asset download (by ID)
/// - `artifact = Some("filename/{name}")` → release asset download (by filename)
/// - `artifact = Some("tarball/{ref}")` → source tarball (github.com/archive/)
/// - `artifact = Some("zipball")` → zip archive (github.com/archive/)
/// - `artifact = Some("raw/{path}")` → raw file (raw.githubusercontent.com)
pub struct GithubRegistryClient {
    pub(super) http: reqwest::Client,
    /// Credentialed download client with redirects **disabled**, so the SSRF
    /// guard validates every hop (RFC 0019 §6.3). Carries the same auth
    /// default headers as `http`.
    pub(super) dl_credentialed: reqwest::Client,
    /// Credential-free download client (redirects disabled) for any hop that
    /// leaves the trusted origins, so the token never leaves them.
    pub(super) dl_plain: reqwest::Client,
    pub(super) base_url: String,
    /// Base URL for raw file downloads (default: `https://raw.githubusercontent.com`).
    pub(super) raw_base_url: String,
    /// Base URL for archive downloads (default: `https://github.com`).
    pub(super) archive_base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// The rate-limit budget every API call draws on (RFC 0019 §5.2).
    pub(super) api: BudgetedApi,
    /// The fingerprint of the configured token, for the budget row.
    pub(super) token_fingerprint: String,
}

/// The asset a coordinate names, from a release's asset list: by filename
/// where the selector is `filename/…`, by numeric id otherwise. `None` when
/// the coordinate names the release rather than one of its assets.
fn selected_asset<'a>(
    assets: &'a [GhAsset],
    artifact: Option<&str>,
) -> Result<Option<&'a GhAsset>, CoreError> {
    let Some(artifact) = artifact else {
        return Ok(None);
    };
    if let Some(filename) = artifact.strip_prefix("filename/") {
        return Ok(assets.iter().find(|a| a.name == filename));
    }
    let asset_id: u64 = artifact
        .parse()
        .map_err(|_| CoreError::Registry(format!("invalid asset id: {artifact}")))?;
    Ok(assets.iter().find(|a| a.id == asset_id))
}

impl GithubRegistryClient {
    /// Raw file and archive downloads name a git ref, not a release, so they
    /// never resolve through one. Once `ProxyService` has resolved that ref
    /// the version is a commit SHA, and the commit is what dates the
    /// coordinate (RFC 0019 §4.2: release → tag → commit). A ref that was not
    /// resolved — no resolver wired — stays undated.
    ///
    /// `None` when the coordinate is not one of those, so the caller carries
    /// on with the release path.
    async fn ref_metadata(
        &self,
        pkg: &PackageId,
        owner_repo: &str,
    ) -> Result<Option<PackageMetadata>, CoreError> {
        let Some(artifact) = pkg.artifact.as_deref() else {
            return Ok(None);
        };
        let names_a_ref = artifact.starts_with("raw/")
            || artifact.starts_with("tarball/")
            || artifact == "zipball";
        if !names_a_ref {
            return Ok(None);
        }
        if !is_commit_sha(&pkg.version) {
            return Ok(Some(PackageMetadata::minimal(
                pkg.clone(),
                serde_json::Value::Null,
            )));
        }
        Ok(Some(commit_dated_metadata(
            pkg,
            self.commit(owner_repo, &pkg.version).await,
        )))
    }

    /// The release tag for a coordinate that carries none.
    ///
    /// `/releases/assets/{id}` with no `?tag=`: the asset's own JSON names its
    /// release through `browser_download_url`
    /// (`…/releases/download/{tag}/{name}`), so the release is looked up from
    /// there. Found by `tests/heavy/mise.sh`: mise addresses an asset by id
    /// alone, and this path answered 404 to it — the placeholder tag was being
    /// looked up as a release.
    async fn tag_of(&self, pkg: &PackageId, owner_repo: &str) -> Result<Option<String>, CoreError> {
        if pkg.version != UNKNOWN_TAG {
            return Ok(None);
        }
        let Some(id) = pkg.artifact.as_deref().and_then(|a| a.parse::<u64>().ok()) else {
            return Ok(None);
        };
        let asset = self.asset_by_id(owner_repo, id).await?;
        let tag = tag_from_download_url(&asset.browser_download_url).ok_or_else(|| {
            CoreError::Registry(format!(
                "asset {id} of {owner_repo}: cannot tell its release from '{}'",
                asset.browser_download_url
            ))
        })?;
        Ok(Some(tag))
    }

    /// The `releases` pseudo-version: every release of the repository, as the
    /// listing the resolver reads.
    async fn release_list_metadata(
        &self,
        pkg: &PackageId,
        owner_repo: &str,
    ) -> Result<PackageMetadata, CoreError> {
        let url = format!("{}/repos/{}/releases", self.base_url, owner_repo);
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo} not found")));
        }
        let releases: Vec<GhRelease> = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        let extra = serde_json::to_value(
            releases
                .iter()
                .map(|r| {
                    serde_json::json!({ "id": r.id, "tag_name": r.tag_name, "published_at": r.published_at })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        Ok(PackageMetadata::minimal(pkg.clone(), extra))
    }

    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        // GitHub-specific default headers merged with any auth headers from opts.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            "application/vnd.github+json"
                .parse()
                .expect("static header value is valid ASCII"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            "2022-11-28"
                .parse()
                .expect("static header value is valid ASCII"),
        );
        let auth_headers = upstream_auth_headers(opts)?;
        headers.extend(auth_headers);

        // Same trio as the Forgejo client: one client for the API, and a
        // credentialed/plain pair with redirects disabled for downloads, so
        // `ssrf::fetch_following_redirects_trusting` validates every hop and
        // drops the token the moment a redirect leaves the trusted origins.
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

        let base_url = base_url.into();
        let token_fingerprint = batlehub_core::ports::token_fingerprint(
            opts.bearer_token
                .as_deref()
                .or(opts.basic_auth.as_ref().map(|(_, p)| p.as_str()))
                .or(opts.custom_header.as_ref().map(|(_, v)| v.as_str())),
        );

        // Derive content URLs from the API base URL.
        // api.github.com → raw.githubusercontent.com / github.com
        // GitHub Enterprise → same host, no /api/v3 prefix
        let (raw_base_url, archive_base_url) = if base_url.contains("api.github.com") {
            (
                "https://raw.githubusercontent.com".to_owned(),
                "https://github.com".to_owned(),
            )
        } else {
            let host = base_url.trim_end_matches('/').trim_end_matches("/api/v3");
            (host.to_owned(), host.to_owned())
        };

        Ok(Self {
            http,
            dl_credentialed,
            dl_plain,
            base_url,
            raw_base_url,
            archive_base_url,
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

    /// The origins the operator's token may be sent to: the API, the archive
    /// host and the raw host, all derived from the configured base URL.
    fn trusted_origins(&self) -> Vec<String> {
        let mut v = vec![
            self.base_url.clone(),
            self.archive_base_url.clone(),
            self.raw_base_url.clone(),
        ];
        v.dedup();
        v
    }

    /// One release asset by id — its name and download URL.
    async fn asset_by_id(&self, owner_repo: &str, asset_id: u64) -> Result<GhAsset, CoreError> {
        let url = format!(
            "{}/repos/{}/releases/assets/{}",
            self.base_url, owner_repo, asset_id
        );
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("asset {asset_id} not found")));
        }
        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<GhAsset>()
            .await
            .map_err(to_registry_error)
    }

    /// The release-by-tag JSON as the forge sent it, streamed.
    async fn release_json(
        &self,
        owner_repo: &str,
        tag: &str,
    ) -> Result<FetchedArtifact, CoreError> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            self.base_url, owner_repo, tag
        );
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo}@{tag} not found")));
        }
        let resp = resp.error_for_status().map_err(to_registry_error)?;
        let cache_control = resp
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(FetchedArtifact {
            stream: Box::pin(resp.bytes_stream().map_err(to_registry_error)),
            cache_control,
        })
    }

    /// The tag object behind an annotated tag: its target commit and tagger.
    async fn tag_object(&self, owner_repo: &str, sha: &str) -> Result<GhTagObject, CoreError> {
        let url = format!("{}/repos/{}/git/tags/{}", self.base_url, owner_repo, sha);
        self.api_get(&url)
            .await?
            .error_for_status()
            .map_err(to_registry_error)?
            .json::<GhTagObject>()
            .await
            .map_err(to_registry_error)
    }

    pub(super) async fn fetch_release_by_tag(
        &self,
        owner_repo: &str,
        tag: &str,
    ) -> Result<GhRelease, CoreError> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            self.base_url, owner_repo, tag
        );
        let resp = self.api_get(&url).await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo}@{tag} not found")));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<GhRelease>()
            .await
            .map_err(to_registry_error)
    }
}

// ── ForgeReleaseSource impl (RFC 0021 §5.2) ───────────────────────────────────

/// GitHub addresses a release asset by file name: the `filename/<name>`
/// sub-coordinate is what `download_asset_by_name` builds and what
/// `selected_asset` reads back, so an import fetching through
/// `RegistryClient::fetch_artifact` takes the same path a client's own download
/// takes — credential, allowlist and SSRF guard included.
fn gh_release(release: GhRelease) -> ForgeRelease {
    ForgeRelease {
        tag: release.tag_name,
        draft: release.draft,
        prerelease: release.prerelease,
        assets: release
            .assets
            .into_iter()
            .map(|a| ForgeAsset {
                artifact: format!("filename/{}", a.name),
                name: a.name,
                size: Some(a.size),
            })
            .collect(),
    }
}

#[async_trait]
impl ForgeReleaseSource for GithubRegistryClient {
    async fn list_releases(&self, repo: &str) -> Result<Vec<ForgeRelease>, CoreError> {
        let url = format!("{}/repos/{}/releases", self.base_url, repo);
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{repo} not found")));
        }
        let releases: Vec<GhRelease> = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(releases.into_iter().map(gh_release).collect())
    }

    async fn release_by_tag(&self, repo: &str, tag: &str) -> Result<ForgeRelease, CoreError> {
        self.fetch_release_by_tag(repo, tag).await.map(gh_release)
    }
}

// ── ForgeRegistry impl (RFC 0019 §6.3) ────────────────────────────────────────

/// GitHub's (and Forgejo's) `verification` object, as a provenance state.
///
/// `verified: true` is the forge's own cryptographic check. `false` with a
/// reason of "not signed" is nothing to report; `false` with any other reason
/// is a signature that failed, which is a different fact and a different
/// code.
fn verification_to_provenance(v: Option<&GhVerification>) -> ForgeProvenance {
    match v {
        Some(v) if v.verified => ForgeProvenance::Verified {
            detail: v.reason.clone().unwrap_or_else(|| "valid".to_owned()),
        },
        Some(v) => {
            let reason = v.reason.clone().unwrap_or_default();
            if reason.is_empty() || reason.contains("not_signed") || reason.contains("unsigned") {
                ForgeProvenance::Missing
            } else {
                ForgeProvenance::Invalid { reason }
            }
        }
        None => ForgeProvenance::Missing,
    }
}

#[async_trait]
impl ForgeRegistry for GithubRegistryClient {
    /// Tag first (`git/ref/tags/{ref}`, then the tag object for an annotated
    /// tag), then branch (`branches/{ref}`), then not found. Confirmed against
    /// api.github.com on 2026-09-03: `cli/cli` `v2.60.0` is a lightweight tag
    /// whose ref object is the commit; `git/git` `v2.45.0` is annotated and
    /// its ref object is a `tag` whose object is the commit; `branches/trunk`
    /// carries the head commit and its committer date.
    async fn resolve_ref(
        &self,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<ResolvedTarget, CoreError> {
        let tag_url = format!(
            "{}/repos/{}/git/ref/tags/{}",
            self.base_url, owner_repo, git_ref
        );
        let resp = self.api_get(&tag_url).await?;
        if resp.status() != reqwest::StatusCode::NOT_FOUND {
            let r: GhRef = resp
                .error_for_status()
                .map_err(to_registry_error)?
                .json()
                .await
                .map_err(to_registry_error)?;
            return if r.object.kind == "tag" {
                let tag = self.tag_object(owner_repo, &r.object.sha).await?;
                Ok(ResolvedTarget {
                    kind: RefKind::Tag,
                    sha: tag.object.sha,
                    object_date: parse_date(tag.tagger.as_ref().and_then(|t| t.date.as_deref())),
                    publisher: tag
                        .tagger
                        .as_ref()
                        .and_then(|t| person_label(None, t.name.as_deref(), t.email.as_deref())),
                })
            } else {
                Ok(ResolvedTarget {
                    kind: RefKind::Tag,
                    sha: r.object.sha,
                    object_date: None,
                    publisher: None,
                })
            };
        }

        let branch_url = format!(
            "{}/repos/{}/branches/{}",
            self.base_url, owner_repo, git_ref
        );
        let resp = self.api_get(&branch_url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{owner_repo}: no tag or branch named '{git_ref}'"
            )));
        }
        let b: GhBranch = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        let committer = b.commit.commit.as_ref().and_then(|c| c.committer.as_ref());
        Ok(ResolvedTarget {
            kind: RefKind::Branch,
            sha: b.commit.sha,
            object_date: parse_date(committer.and_then(|c| c.date.as_deref())),
            publisher: committer
                .and_then(|c| person_label(None, c.name.as_deref(), c.email.as_deref())),
        })
    }

    /// `GET /repos/{o}/{r}/tags` — name and commit sha, newest first as
    /// GitHub orders them. The list carries no date, so an annotated tag's
    /// tagger date is not here; `resolve_ref` is what fetches one when a
    /// coordinate needs it, and paying two calls per tag to fill a listing
    /// would spend the whole rate-limit budget on a display.
    async fn tags(&self, owner_repo: &str) -> Result<Vec<ForgeTag>, CoreError> {
        let url = format!("{}/repos/{}/tags?per_page=100", self.base_url, owner_repo);
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{owner_repo} not found")));
        }
        let tags: Vec<GhTag> = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(tags
            .into_iter()
            .map(|t| ForgeTag {
                name: t.name,
                sha: t.commit.sha,
                date: None,
            })
            .collect())
    }

    /// An attestation for the asset's digest when the coordinate names one,
    /// the commit's signature otherwise (RFC 0019 phase 5).
    ///
    /// **Never `Unverifiable`** — that state is GitLab's release evidence and
    /// nothing else (decision 8); a test below asserts it.
    ///
    /// GitHub Enterprise below 3.13 has no attestations endpoint and answers
    /// `404`; that is `Missing`, which is the same thing the endpoint's empty
    /// array means and the honest answer either way.
    async fn provenance(
        &self,
        owner_repo: &str,
        sha: &str,
        asset_digest: Option<&str>,
    ) -> Result<ForgeProvenance, CoreError> {
        if let Some(digest) = asset_digest {
            let digest = if digest.contains(':') {
                digest.to_owned()
            } else {
                format!("sha256:{digest}")
            };
            let url = format!(
                "{}/repos/{}/attestations/{}",
                self.base_url, owner_repo, digest
            );
            let resp = self.api_get(&url).await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(ForgeProvenance::Missing);
            }
            let a: GhAttestations = resp
                .error_for_status()
                .map_err(to_registry_error)?
                .json()
                .await
                .map_err(to_registry_error)?;
            return Ok(if a.attestations.is_empty() {
                ForgeProvenance::Missing
            } else {
                ForgeProvenance::Verified {
                    detail: format!("{} build attestation(s) for {digest}", a.attestations.len()),
                }
            });
        }
        let url = format!("{}/repos/{}/commits/{}", self.base_url, owner_repo, sha);
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(ForgeProvenance::Missing);
        }
        let c: GhCommit = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(to_registry_error)?;
        Ok(verification_to_provenance(
            c.commit.as_ref().and_then(|d| d.verification.as_ref()),
        ))
    }

    async fn commit(&self, owner_repo: &str, sha: &str) -> Result<ForgeCommit, CoreError> {
        let url = format!("{}/repos/{}/commits/{}", self.base_url, owner_repo, sha);
        let resp = self.api_get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{owner_repo}: no commit {sha}"
            )));
        }
        let c: GhCommit = resp
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

/// Metadata for an archive or raw coordinate at a resolved commit: the
/// commit's date and committer under `extra.forge`.
///
/// **Fails open on the lookup**: a commit the forge cannot describe (an API
/// blip, a budget refusal) is served undated, as it always was, rather than
/// refused — the ref already resolved, so the bytes exist. The age gate then
/// does what its `deny_missing_timestamp` says, which is the operator's call.
pub(crate) fn commit_dated_metadata(
    pkg: &PackageId,
    commit: Result<ForgeCommit, CoreError>,
) -> PackageMetadata {
    match commit {
        Ok(c) => PackageMetadata {
            id: pkg.clone(),
            published_at: c.committed_at,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({
                FORGE_EXTRA_KEY: {
                    "resolved_commit": c.sha,
                    "committed_at": c.committed_at,
                    "committer": c.committer,
                }
            }),
            cache_control: None,
        },
        Err(e) => {
            tracing::warn!(
                coordinate = %pkg,
                error = %e,
                "could not date the commit; serving the coordinate undated"
            );
            PackageMetadata::minimal(pkg.clone(), serde_json::Value::Null)
        }
    }
}

// ── Pure helper functions (also used by models.rs RegistryClient impl) ────────

pub(super) fn is_release_signed(assets: &[GhAsset]) -> bool {
    assets
        .iter()
        .any(|a| a.name.ends_with(".asc") || a.name.ends_with(".sig"))
}

/// The tag in a `browser_download_url` — `…/releases/download/{tag}/{name}`.
pub(super) fn tag_from_download_url(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("/releases/download/")?;
    let tag = rest.split('/').next()?;
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// Build a direct download URL for non-API artifact types (tarball, zipball, raw).
pub(super) fn static_artifact_url(
    artifact: &str,
    archive_base: &str,
    raw_base: &str,
    owner_repo: &str,
    git_ref: &str,
) -> Option<String> {
    if artifact.starts_with("tarball/") {
        Some(format!(
            "{}/{}/archive/{}.tar.gz",
            archive_base, owner_repo, git_ref
        ))
    } else if artifact == "zipball" {
        Some(format!(
            "{}/{}/archive/{}.zip",
            archive_base, owner_repo, git_ref
        ))
    } else {
        artifact
            .strip_prefix("raw/")
            .map(|file_path| format!("{}/{}/{}/{}", raw_base, owner_repo, git_ref, file_path))
    }
}

/// Parse the `Link` header and return the URL for `rel="next"`, if present.
pub(super) fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        let mut url_part = None;
        let mut is_next = false;
        for segment in part.split(';') {
            let s = segment.trim();
            if s.starts_with('<') && s.ends_with('>') {
                url_part = Some(s[1..s.len() - 1].to_owned());
            } else if s == r#"rel="next""# {
                is_next = true;
            }
        }
        if is_next {
            return url_part;
        }
    }
    None
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for GithubRegistryClient {
    fn releases(&self) -> Option<&dyn batlehub_core::ports::ForgeReleaseSource> {
        Some(self)
    }

    fn registry_type(&self) -> &str {
        "github"
    }

    fn forge(&self) -> Option<&dyn ForgeRegistry> {
        Some(self)
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let url = format!("{}/repos/{}/releases", self.base_url, package);
        fetch_release_listing(self.get(&url), kind, "github", "GitHub", package).await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let owner_repo = &pkg.name;

        if let Some(meta) = self.ref_metadata(pkg, owner_repo).await? {
            return Ok(meta);
        }

        let tag_owned;
        let version = match self.tag_of(pkg, owner_repo).await? {
            Some(tag) => {
                tag_owned = tag;
                tag_owned.as_str()
            }
            None => pkg.version.as_str(),
        };

        match version {
            "releases" => self.release_list_metadata(pkg, owner_repo).await,

            tag => {
                let release = self.fetch_release_by_tag(owner_repo, tag).await?;

                let published_at = release
                    .published_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));

                let is_signed = is_release_signed(&release.assets);

                // The asset this coordinate names, found once: the URL to
                // stream and the digest RFC 0019's `ASSET_REPLACED` compares
                // come from the same object, and finding it twice was how
                // the two could disagree.
                let selected = selected_asset(&release.assets, pkg.artifact.as_deref())?;
                let download_url = selected.map(|a| a.browser_download_url.clone());
                let selected_digest = selected.and_then(|a| a.digest.clone());

                let extra = serde_json::json!({
                    "release_id": release.id,
                    "tag_name": release.tag_name,
                    "assets": release.assets.iter().map(|a| serde_json::json!({
                        "id": a.id,
                        "name": a.name,
                        "download_url": a.browser_download_url,
                        "digest": a.digest,
                    })).collect::<Vec<_>>(),
                    // RFC 0019 phase 2: the digest of the asset this
                    // coordinate names, where the coordinate names one, so
                    // `ASSET_REPLACED` can compare it with the bytes already
                    // cached. A release (no asset selector) carries none.
                    FORGE_EXTRA_KEY: selected_digest
                        .as_ref()
                        .map(|d| serde_json::json!({ FORGE_ASSET_DIGEST: d }))
                        .unwrap_or(serde_json::Value::Null),
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

        let download_url = if let Some(artifact) = &pkg.artifact {
            if let Some(url) = static_artifact_url(
                artifact,
                &self.archive_base_url,
                &self.raw_base_url,
                owner_repo,
                git_ref,
            ) {
                url
            } else if let Some(filename) = artifact.strip_prefix("filename/") {
                let release = self.fetch_release_by_tag(owner_repo, git_ref).await?;
                release
                    .assets
                    .iter()
                    .find(|a| a.name == filename)
                    .map(|a| a.browser_download_url.clone())
                    .ok_or_else(|| {
                        CoreError::NotFound(format!(
                            "no asset named '{filename}' in {owner_repo}@{git_ref}"
                        ))
                    })?
            } else {
                let asset_id: u64 = artifact
                    .parse()
                    .map_err(|_| CoreError::Registry(format!("invalid asset id: {artifact}")))?;
                self.asset_by_id(owner_repo, asset_id)
                    .await?
                    .browser_download_url
            }
        } else if git_ref == "releases" {
            return Err(CoreError::Registry(
                "the release listing is a document, not an artifact".to_owned(),
            ));
        } else {
            // `GET /{o}/{r}/releases/tags/{tag}` — the release's own JSON, as the
            // forge sent it. Found by `tests/heavy/mise.sh`: mise's `github:`
            // backend asks for exactly this before anything else, and this arm
            // used to refuse it, so every install fell back to paging the whole
            // listing. Streamed as received rather than re-serialised from the
            // reduced `GhRelease`, so a client reading a field this proxy does
            // not model still finds it.
            return self.release_json(owner_repo, git_ref).await;
        };

        // A release asset's `browser_download_url` comes from the release JSON,
        // which the repository owner controls. The token goes only to the
        // origins derived from the configured base URL; the initial URL has to
        // be on one of them, and every redirect hop is checked against the
        // SSRF guard and re-issued without credentials once it leaves them
        // (RFC 0019 §6.3 — the one shipped defect the RFC's review found).
        let trusted = self.trusted_origins();
        if !trusted
            .iter()
            .any(|origin| ensure_same_origin(&download_url, origin).is_ok())
        {
            return Err(CoreError::Registry(format!(
                "refusing to fetch '{download_url}': not on this registry's API, archive or raw host"
            )));
        }

        tracing::debug!(url = %download_url, "fetching GitHub artifact");

        let parsed = reqwest::Url::parse(&download_url)
            .map_err(|e| CoreError::Registry(format!("invalid download URL: {e}")))?;
        let response = ssrf::fetch_following_redirects_trusting(
            &self.dl_credentialed,
            &self.dl_plain,
            &self.basic_auth,
            &trusted,
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
        let mut url = format!("{}/repos/{}/releases?per_page=100", self.base_url, package);
        let mut versions = Vec::new();

        for _ in 0..10 {
            let resp = self.api_get(&url).await?;

            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(vec![]);
            }
            if !resp.status().is_success() {
                return Err(CoreError::Registry(format!(
                    "github: releases list returned {}",
                    resp.status()
                )));
            }

            let next_url = next_link(resp.headers());

            let releases: Vec<GhRelease> = resp.json().await.map_err(to_registry_error)?;

            for r in releases {
                versions.push(r.tag_name);
            }

            match next_url {
                Some(next) => url = next,
                None => break,
            }
        }

        Ok(versions)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::ports::RegistryClient;

    fn asset(id: u64, name: &str) -> GhAsset {
        GhAsset {
            id,
            name: name.to_string(),
            browser_download_url: format!("https://example.com/{name}"),
            size: 0,
            digest: None,
        }
    }

    #[test]
    fn is_signed_true_when_asc_present() {
        let assets = vec![asset(1, "binary.tar.gz"), asset(2, "binary.tar.gz.asc")];
        assert!(is_release_signed(&assets));
    }

    #[test]
    fn is_signed_true_when_sig_present() {
        let assets = vec![asset(1, "binary.zip"), asset(2, "binary.zip.sig")];
        assert!(is_release_signed(&assets));
    }

    #[test]
    fn is_signed_false_when_no_sig_asset() {
        let assets = vec![asset(1, "binary.tar.gz"), asset(2, "checksums.txt")];
        assert!(!is_release_signed(&assets));
    }

    #[test]
    fn static_url_tarball() {
        let url = static_artifact_url(
            "tarball/main",
            "https://github.com",
            "https://raw.githubusercontent.com",
            "owner/repo",
            "v1.0",
        );
        assert_eq!(
            url.as_deref(),
            Some("https://github.com/owner/repo/archive/v1.0.tar.gz")
        );
    }

    #[test]
    fn static_url_zipball() {
        let url = static_artifact_url(
            "zipball",
            "https://github.com",
            "https://raw.githubusercontent.com",
            "owner/repo",
            "v1.0",
        );
        assert_eq!(
            url.as_deref(),
            Some("https://github.com/owner/repo/archive/v1.0.zip")
        );
    }

    #[test]
    fn static_url_raw_file() {
        let url = static_artifact_url(
            "raw/src/main.rs",
            "https://github.com",
            "https://raw.githubusercontent.com",
            "owner/repo",
            "main",
        );
        assert_eq!(
            url.as_deref(),
            Some("https://raw.githubusercontent.com/owner/repo/main/src/main.rs")
        );
    }

    #[test]
    fn static_url_none_for_asset_id() {
        let url = static_artifact_url(
            "12345678",
            "https://github.com",
            "https://raw.githubusercontent.com",
            "owner/repo",
            "v1.0",
        );
        assert!(url.is_none());
    }

    #[test]
    fn next_link_parses_rel_next() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            reqwest::header::LINK,
            r#"<https://api.github.com/repos/owner/repo/releases?page=2&per_page=100>; rel="next", <https://api.github.com/repos/owner/repo/releases?page=5&per_page=100>; rel="last""#
                .parse()
                .unwrap(),
        );
        assert_eq!(
            next_link(&map).as_deref(),
            Some("https://api.github.com/repos/owner/repo/releases?page=2&per_page=100")
        );
    }

    #[test]
    fn next_link_absent_when_no_next_rel() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            reqwest::header::LINK,
            r#"<https://api.github.com/repos/owner/repo/releases?page=5&per_page=100>; rel="last""#
                .parse()
                .unwrap(),
        );
        assert!(next_link(&map).is_none());
    }

    #[tokio::test]
    async fn list_versions_single_page() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::to_string(&serde_json::json!([
            { "id": 1, "tag_name": "v1.1.0", "published_at": "2024-01-02T00:00:00Z", "assets": [] },
            { "id": 2, "tag_name": "v1.0.0", "published_at": "2024-01-01T00:00:00Z", "assets": [] },
        ]))
        .unwrap();
        let _mock = server
            .mock("GET", "/repos/owner/repo/releases?per_page=100")
            .with_status(200)
            .with_body(&body)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = GithubRegistryClient::new(server.url(), &opts).unwrap();
        let versions = client.list_versions("owner/repo").await.unwrap();
        assert_eq!(versions, vec!["v1.1.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn list_versions_follows_pagination() {
        let mut server = mockito::Server::new_async().await;

        let page1 = serde_json::to_string(&serde_json::json!([
            { "id": 1, "tag_name": "v1.2.0", "published_at": null, "assets": [] }
        ]))
        .unwrap();
        let page2 = serde_json::to_string(&serde_json::json!([
            { "id": 2, "tag_name": "v1.1.0", "published_at": null, "assets": [] },
            { "id": 3, "tag_name": "v1.0.0", "published_at": null, "assets": [] }
        ]))
        .unwrap();

        let page2_url = format!(
            "{}/repos/owner/repo/releases?page=2&per_page=100",
            server.url()
        );
        let link_header = format!(r#"<{page2_url}>; rel="next""#);

        let _m1 = server
            .mock("GET", "/repos/owner/repo/releases?per_page=100")
            .with_status(200)
            .with_header("link", &link_header)
            .with_body(&page1)
            .create_async()
            .await;
        let _m2 = server
            .mock("GET", "/repos/owner/repo/releases?page=2&per_page=100")
            .with_status(200)
            .with_body(&page2)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = GithubRegistryClient::new(server.url(), &opts).unwrap();
        let versions = client.list_versions("owner/repo").await.unwrap();
        assert_eq!(versions, vec!["v1.2.0", "v1.1.0", "v1.0.0"]);
    }

    #[tokio::test]
    async fn list_versions_404_returns_empty() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/repos/unknown/repo/releases?per_page=100")
            .with_status(404)
            .create_async()
            .await;

        let opts = UpstreamHttpOptions::default();
        let client = GithubRegistryClient::new(server.url(), &opts).unwrap();
        let versions = client.list_versions("unknown/repo").await.unwrap();
        assert!(versions.is_empty());
    }
}

// ── RFC 0019: refs, commits, the SSRF guard and the budget ───────────────────

#[cfg(test)]
mod forge_tests {
    use super::*;
    use crate::in_memory::InMemoryRateLimitBudget;
    use batlehub_core::ports::{ForgeRegistry, RateLimitBudget};
    use mockito::Server;

    const COMMIT: &str = "45437bc7eeeb3359bbfddd1742f79de7652fd3e2";
    const TAG_OBJ: &str = "8be58deda2ccce7d036072ec34439f9fad88204f";

    fn client(server: &Server) -> GithubRegistryClient {
        GithubRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap()
    }

    /// The three facts an import chooses a release by, out of the shape
    /// api.github.com returns for `/repos/{owner}/{repo}/releases`
    /// (RFC 0021 §5.2). `draft` and `prerelease` are the fields this model did
    /// not carry until the import needed them.
    #[tokio::test]
    async fn releases_are_normalised_with_their_draft_and_prerelease_flags() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/repos/acme/ext/releases")
            .with_body(
                r#"[
                  {"id":3,"tag_name":"v3","draft":true,"prerelease":false,"assets":[]},
                  {"id":2,"tag_name":"v2","draft":false,"prerelease":true,"assets":[]},
                  {"id":1,"tag_name":"v1","draft":false,"prerelease":false,
                   "assets":[{"id":9,"name":"ext-1.0.0.vsix","size":12,
                              "browser_download_url":"https://example.invalid/ext-1.0.0.vsix"}]}
                ]"#,
            )
            .create_async()
            .await;

        let got = client(&server).list_releases("acme/ext").await.unwrap();

        assert_eq!(got.len(), 3);
        assert!(got[0].draft && !got[0].is_stable());
        assert!(got[1].prerelease && !got[1].is_stable());
        assert!(got[2].is_stable(), "the one `latest` may choose");
        // The sub-coordinate is the one the download route builds, so an
        // import fetches by the path a client's own download takes.
        assert_eq!(got[2].assets[0].artifact, "filename/ext-1.0.0.vsix");
        assert_eq!(got[2].assets[0].size, Some(12));
    }

    /// A release older than the first page is reached by name, not by paging.
    #[tokio::test]
    async fn a_release_is_reachable_by_tag() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/repos/acme/ext/releases/tags/v0.1.0")
            .with_body(r#"{"id":1,"tag_name":"v0.1.0","prerelease":true,"assets":[]}"#)
            .create_async()
            .await;

        let got = client(&server)
            .release_by_tag("acme/ext", "v0.1.0")
            .await
            .unwrap();

        assert_eq!(got.tag, "v0.1.0");
        assert!(got.prerelease);
        // Absent in the response and defaulted, rather than failing the decode:
        // a strictly-modelled release has broken a real client here before.
        assert!(!got.draft);
    }

    /// The shape api.github.com returned for `cli/cli` `v2.60.0` on 2026-09-03.
    #[tokio::test]
    async fn a_lightweight_tag_resolves_from_the_ref_object() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/repos/cli/cli/git/ref/tags/v2.60.0")
            .with_body(format!(
                r#"{{"ref":"refs/tags/v2.60.0","object":{{"sha":"{COMMIT}","type":"commit"}}}}"#
            ))
            .create_async()
            .await;
        let t = client(&server)
            .resolve_ref("cli/cli", "v2.60.0")
            .await
            .unwrap();
        assert_eq!(t.kind, RefKind::Tag);
        assert_eq!(t.sha, COMMIT);
        assert!(t.object_date.is_none(), "a lightweight tag has no tagger");
    }

    /// The shape api.github.com returned for `git/git` `v2.45.0`: the ref
    /// points at a tag object, and the tag object at the commit.
    #[tokio::test]
    async fn an_annotated_tag_resolves_through_the_tag_object_to_its_commit() {
        let mut server = Server::new_async().await;
        let _r = server
            .mock("GET", "/repos/git/git/git/ref/tags/v2.45.0")
            .with_body(format!(
                r#"{{"ref":"refs/tags/v2.45.0","object":{{"sha":"{TAG_OBJ}","type":"tag"}}}}"#
            ))
            .create_async()
            .await;
        let _t = server
            .mock("GET", &*format!("/repos/git/git/git/tags/{TAG_OBJ}"))
            .with_body(format!(
                r#"{{"sha":"{TAG_OBJ}","tag":"v2.45.0",
                    "tagger":{{"name":"Junio C Hamano","email":"gitster@pobox.com","date":"2024-04-29T14:30:43Z"}},
                    "object":{{"sha":"{COMMIT}","type":"commit"}}}}"#
            ))
            .create_async()
            .await;
        let t = client(&server)
            .resolve_ref("git/git", "v2.45.0")
            .await
            .unwrap();
        assert_eq!(t.kind, RefKind::Tag);
        assert_eq!(t.sha, COMMIT, "the commit, not the tag object");
        assert_eq!(
            t.object_date.map(|d| d.to_rfc3339()),
            Some("2024-04-29T14:30:43+00:00".to_owned())
        );
        assert_eq!(
            t.publisher.as_deref(),
            Some("Junio C Hamano <gitster@pobox.com>")
        );
    }

    #[tokio::test]
    async fn a_branch_resolves_after_the_tag_lookup_misses() {
        let mut server = Server::new_async().await;
        let _miss = server
            .mock("GET", "/repos/cli/cli/git/ref/tags/trunk")
            .with_status(404)
            .create_async()
            .await;
        let _b = server
            .mock("GET", "/repos/cli/cli/branches/trunk")
            .with_body(format!(
                r#"{{"name":"trunk","commit":{{"sha":"{COMMIT}","commit":{{"committer":{{"name":"GitHub","email":"noreply@github.com","date":"2026-09-03T15:24:19Z"}}}}}}}}"#
            ))
            .create_async()
            .await;
        let t = client(&server)
            .resolve_ref("cli/cli", "trunk")
            .await
            .unwrap();
        assert_eq!(t.kind, RefKind::Branch);
        assert_eq!(t.sha, COMMIT);
        assert_eq!(
            t.object_date.map(|d| d.to_rfc3339()),
            Some("2026-09-03T15:24:19+00:00".to_owned())
        );
    }

    #[tokio::test]
    async fn a_ref_that_is_neither_is_not_found() {
        let mut server = Server::new_async().await;
        let _t = server
            .mock("GET", "/repos/o/r/git/ref/tags/nope")
            .with_status(404)
            .create_async()
            .await;
        let _b = server
            .mock("GET", "/repos/o/r/branches/nope")
            .with_status(404)
            .create_async()
            .await;
        let err = client(&server)
            .resolve_ref("o/r", "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "{err}");
    }

    #[tokio::test]
    async fn a_commit_is_dated_by_its_committer_and_named_by_its_login() {
        let mut server = Server::new_async().await;
        let _c = server
            .mock("GET", &*format!("/repos/o/r/commits/{COMMIT}"))
            .with_body(format!(
                r#"{{"sha":"{COMMIT}","commit":{{"committer":{{"name":"William","email":"w@x","date":"2026-09-03T15:24:19Z"}}}},"committer":{{"login":"williammartin"}}}}"#
            ))
            .create_async()
            .await;
        let c = client(&server).commit("o/r", COMMIT).await.unwrap();
        assert_eq!(c.committer.as_deref(), Some("williammartin"));
        assert!(c.committed_at.is_some());
    }

    /// An archive coordinate at a resolved commit is dated by that commit —
    /// the metadata contract RFC 0018's age gate consumes.
    #[tokio::test]
    async fn an_archive_at_a_commit_sha_carries_the_commit_date() {
        let mut server = Server::new_async().await;
        let _c = server
            .mock("GET", &*format!("/repos/o/r/commits/{COMMIT}"))
            .with_body(format!(
                r#"{{"sha":"{COMMIT}","commit":{{"committer":{{"name":"W","email":"w@x","date":"2026-09-03T15:24:19Z"}}}}}}"#
            ))
            .create_async()
            .await;
        let pkg = PackageId::new("gh", "o/r", COMMIT).with_artifact(format!("tarball/{COMMIT}"));
        let meta = client(&server).resolve_metadata(&pkg).await.unwrap();
        assert!(meta.published_at.is_some());
        assert_eq!(meta.extra["forge"]["resolved_commit"], COMMIT);

        // An unresolved ref stays undated, exactly as before.
        let pkg = PackageId::new("gh", "o/r", "main").with_artifact("tarball/main");
        let meta = client(&server).resolve_metadata(&pkg).await.unwrap();
        assert!(meta.published_at.is_none());
    }

    /// The one shipped defect RFC 0019's review found: the GitHub client
    /// followed redirects unguarded. A redirect from the archive host to a
    /// private address is now refused.
    #[tokio::test]
    async fn a_redirect_to_a_private_address_is_refused() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", "/o/r/archive/main.tar.gz")
            .with_status(302)
            .with_header("location", "http://169.254.169.254/latest/meta-data/")
            .create_async()
            .await;
        // The mock server is the configured base, so it is also the archive host.
        let pkg = PackageId::new("gh", "o/r", "main").with_artifact("tarball/main");
        let err = match client(&server).fetch_artifact(&pkg).await {
            Ok(_) => panic!("a redirect to a link-local address was followed"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("SSRF guard"), "{err}");
    }

    /// …and a redirect between the trusted origins keeps the token — a private
    /// repository's archive redirects from the API host to the archive host and
    /// needs it on both. The off-origin half (credentials dropped, hop checked)
    /// is `ssrf::fetch_following_redirects`' own contract, unchanged here.
    #[tokio::test]
    async fn a_redirect_between_trusted_origins_keeps_the_token() {
        let mut origin = Server::new_async().await;
        let mut archive = Server::new_async().await;
        let _hop = origin
            .mock("GET", "/o/r/archive/main.tar.gz")
            .match_header("authorization", "Bearer ghp_secret")
            .with_status(302)
            .with_header("location", &format!("{}/blob", archive.url()))
            .create_async()
            .await;
        let landed = archive
            .mock("GET", "/blob")
            .match_header("authorization", "Bearer ghp_secret")
            .with_body("TARBALL")
            .create_async()
            .await;
        let opts = UpstreamHttpOptions {
            bearer_token: Some("ghp_secret".into()),
            ..Default::default()
        };
        let mut c = GithubRegistryClient::new(origin.url(), &opts).unwrap();
        c.raw_base_url = archive.url();
        let pkg = PackageId::new("gh", "o/r", "main").with_artifact("tarball/main");
        let fetched = c.fetch_artifact(&pkg).await.unwrap();
        let body: Vec<u8> = fetched
            .stream
            .try_fold(Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(body, b"TARBALL");
        landed.assert_async().await;
    }

    /// mise's second request: the asset by id, with no tag. The asset's own
    /// JSON says which release it belongs to.
    #[tokio::test]
    async fn an_asset_by_id_alone_finds_its_release_through_its_download_url() {
        let mut server = Server::new_async().await;
        let _asset = server
            .mock("GET", "/repos/o/r/releases/assets/201497623")
            .with_body(format!(
                r#"{{"id":201497623,"name":"gh.tar.gz","browser_download_url":"{}/o/r/releases/download/v2.60.0/gh.tar.gz","size":3}}"#,
                server.url()
            ))
            .expect_at_least(1)
            .create_async()
            .await;
        let _rel = server
            .mock("GET", "/repos/o/r/releases/tags/v2.60.0")
            .with_body(
                r#"{"id":1,"tag_name":"v2.60.0","published_at":"2026-08-01T00:00:00Z","assets":[]}"#,
            )
            .create_async()
            .await;
        let _bytes = server
            .mock("GET", "/o/r/releases/download/v2.60.0/gh.tar.gz")
            .with_body("GH")
            .create_async()
            .await;
        let pkg = PackageId::new("gh", "o/r", UNKNOWN_TAG).with_artifact("201497623");
        let c = client(&server);
        let meta = c.resolve_metadata(&pkg).await.unwrap();
        assert!(
            meta.published_at.is_some(),
            "dated by the release it belongs to"
        );
        assert_eq!(meta.extra["tag_name"], "v2.60.0");
        let fetched = c.fetch_artifact(&pkg).await.unwrap();
        let got: Vec<u8> = fetched
            .stream
            .try_fold(Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(got, b"GH");
        assert_eq!(
            tag_from_download_url("https://github.com/cli/cli/releases/download/v2.60.0/x.tgz")
                .as_deref(),
            Some("v2.60.0")
        );
        assert_eq!(tag_from_download_url("https://example.invalid/x"), None);
    }

    /// The route mise's `github:` backend calls first, and the one that used
    /// to answer 500 for every real client.
    #[tokio::test]
    async fn a_release_by_tag_streams_the_forges_own_json() {
        let mut server = Server::new_async().await;
        let body = r#"{"id":1,"tag_name":"v1","prerelease":false,"assets":[]}"#;
        let _rel = server
            .mock("GET", "/repos/o/r/releases/tags/v1")
            .with_body(body)
            .create_async()
            .await;
        let pkg = PackageId::new("gh", "o/r", "v1");
        let fetched = client(&server).fetch_artifact(&pkg).await.unwrap();
        let got: Vec<u8> = fetched
            .stream
            .try_fold(Vec::new(), |mut a, c| async move {
                a.extend_from_slice(&c);
                Ok(a)
            })
            .await
            .unwrap();
        assert_eq!(got, body.as_bytes(), "byte-exact, `prerelease` included");
    }

    #[tokio::test]
    async fn an_asset_url_off_the_trusted_hosts_is_refused_before_any_request() {
        let mut server = Server::new_async().await;
        let _rel = server
            .mock("GET", "/repos/o/r/releases/tags/v1")
            .with_body(
                r#"{"id":1,"tag_name":"v1","published_at":null,
                    "assets":[{"id":9,"name":"app.bin","browser_download_url":"https://evil.example/app.bin","size":3}]}"#,
            )
            .create_async()
            .await;
        let pkg = PackageId::new("gh", "o/r", "v1").with_artifact("filename/app.bin");
        let err = match client(&server).fetch_artifact(&pkg).await {
            Ok(_) => panic!("an off-host asset URL was fetched"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("not on this registry"), "{err}");
    }

    /// Every API answer's `X-RateLimit-*` lands in the shared budget, and a
    /// budget below the proxy's reserve refuses the next call without making it.
    #[tokio::test]
    async fn api_calls_observe_the_budget_and_refuse_below_the_reserve() {
        let mut server = Server::new_async().await;
        let ref_mock = server
            .mock("GET", "/repos/o/r/git/ref/tags/v1")
            .with_header("x-ratelimit-remaining", "3")
            .with_header("x-ratelimit-limit", "60")
            .with_header(
                "x-ratelimit-reset",
                &(chrono::Utc::now().timestamp() + 600).to_string(),
            )
            .with_body(format!(
                r#"{{"object":{{"sha":"{COMMIT}","type":"commit"}}}}"#
            ))
            .expect(1)
            .create_async()
            .await;
        let budget = InMemoryRateLimitBudget::new();
        let c = client(&server).with_budget("gh", Arc::clone(&budget) as Arc<dyn RateLimitBudget>);

        c.resolve_ref("o/r", "v1").await.unwrap();
        let fp = c.token_fingerprint.clone();
        let obs = budget.observed("gh", &fp).await.unwrap();
        assert_eq!((obs.remaining, obs.limit), (3, 60));

        // 3 of 60 is under the proxy's 10 % reserve: refused, and not called.
        let err = c.resolve_ref("o/r", "v1").await.unwrap_err();
        assert!(err.to_string().contains("rate-limit budget"), "{err}");
        ref_mock.assert_async().await;
    }
}
