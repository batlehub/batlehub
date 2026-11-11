use crate::registry::encode_path;
use async_trait::async_trait;
use futures::TryStreamExt;

use super::{modules, providers, TerraformRegistryClient};
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
};

use super::super::http_client::{cache_control, fetch_json_document, to_registry_error};

impl TerraformRegistryClient {
    /// Follow a URL named inside a provider's download document.
    ///
    /// `shasums_url` and `shasums_signature_url` are what Terraform verifies the
    /// provider archive against. Left pointing upstream they make an otherwise
    /// air-gapped install reach the internet at the last step — the archive is
    /// gated and cached, and then its checksums are not (RFC 0009 §12.8).
    ///
    /// Same SSRF treatment as the module tarball: the target host comes from
    /// upstream, so every hop is validated with the configured upstream as the
    /// only trusted origin.
    async fn fetch_provider_sidecar(
        &self,
        download_doc_url: &str,
        field: &str,
        pkg: &PackageId,
    ) -> Result<FetchedArtifact, CoreError> {
        let doc: serde_json::Value = self
            .get(download_doc_url)
            .send()
            .await
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(|e| {
                CoreError::Registry(format!(
                    "terraform: parsing provider download document: {e}"
                ))
            })?;

        let target = doc
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "terraform provider download document for {} names no '{field}'",
                    pkg.cache_key()
                ))
            })?
            .to_owned();

        let resolved = reqwest::Url::parse(&target)
            .map_err(|e| CoreError::Registry(format!("terraform: bad '{field}' URL: {e}")))?;
        if !matches!(resolved.scheme(), "http" | "https") {
            return Err(CoreError::NotSupported(format!(
                "terraform '{field}' is a '{}' URL, which this proxy cannot fetch",
                resolved.scheme()
            )));
        }

        self.stream_resolved(resolved, pkg, &format!("'{field}'"))
            .await
    }

    /// Fetch an already-resolved absolute URL and wrap it as an artifact stream.
    ///
    /// Both callers arrive here the same way: a Terraform pointer names an
    /// absolute URL — the provider download document's `download_url`, or a
    /// module's `X-Terraform-Get` — and from there the fetch is identical.
    /// `what` names the pointer in the status error.
    ///
    /// The client is built with `Policy::none()` because
    /// `ssrf::fetch_following_redirects` walks the redirect chain itself, so
    /// that every hop is checked rather than only the first.
    async fn stream_resolved(
        &self,
        resolved: reqwest::Url,
        pkg: &PackageId,
        what: &str,
    ) -> Result<FetchedArtifact, CoreError> {
        let plain = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CoreError::Registry(format!("terraform: building client: {e}")))?;

        let response = super::super::ssrf::fetch_following_redirects(
            &plain,
            &plain,
            &self.basic_auth,
            &self.base_url,
            resolved,
        )
        .await?;

        if !response.status().is_success() {
            return Err(CoreError::Registry(format!(
                "terraform {what} returned {} for {}",
                response.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&response);
        let stream = response.bytes_stream().map_err(to_registry_error);
        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    /// Resolve a module's `X-Terraform-Get` pointer and stream what it names.
    ///
    /// Only `http`/`https` targets are followed. `X-Terraform-Get` is a
    /// go-getter source, so it may legitimately be `git::ssh://…` or
    /// `s3::https://…` — those are not a tarball this proxy can fetch, cache and
    /// gate, and pretending otherwise would produce a corrupt artifact rather
    /// than an honest error.
    ///
    /// The target host is upstream-controlled, so every hop goes through
    /// [`ssrf::fetch_following_redirects`] with the configured upstream as the
    /// trusted origin — the same treatment PyPI, GitLab and Forgejo give their
    /// cross-origin download URLs.
    async fn fetch_module_tarball(
        &self,
        download_url: &str,
        pkg: &PackageId,
    ) -> Result<FetchedArtifact, CoreError> {
        let pointer = self
            .get(download_url)
            .send()
            .await
            .map_err(to_registry_error)?;

        if pointer.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "terraform module not found: {}",
                pkg.cache_key()
            )));
        }

        let target = pointer
            .headers()
            .get("X-Terraform-Get")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                CoreError::Registry(format!(
                    "terraform upstream returned no X-Terraform-Get for {}",
                    pkg.cache_key()
                ))
            })?;

        // go-getter prefixes the transport onto the URL: `git::https://…`. When
        // what follows is plain http(s) the archive is fetchable, so the prefix
        // is stripped and the URL used as-is — the refusal below is for the
        // transports that really are clones (`git::ssh://`, `hg::`, `s3::`).
        let target = target
            .strip_prefix("git::")
            .filter(|rest| rest.starts_with("http://") || rest.starts_with("https://"))
            .map(str::to_owned)
            .unwrap_or(target);

        // Relative targets are resolved against the download endpoint, which is
        // what the protocol specifies and what registry.terraform.io emits.
        let base = reqwest::Url::parse(download_url)
            .map_err(|e| CoreError::Registry(format!("terraform: bad download URL: {e}")))?;
        let resolved = base.join(&target).map_err(|e| {
            CoreError::Registry(format!("terraform: bad X-Terraform-Get '{target}': {e}"))
        })?;

        // A go-getter `git::https://…` source names a repository this proxy may
        // already mirror as a forge registry, where the archive *is* a fetchable
        // artifact — so it is worth saying so rather than only refusing.
        if !matches!(resolved.scheme(), "http" | "https") {
            let host = resolved.host_str().unwrap_or("the source host");
            return Err(CoreError::NotSupported(format!(
                "terraform module {} is published as a '{}' go-getter source, which is a \
                 clone rather than a fetchable archive: caching one would mean archiving a \
                 working tree and serving bytes no upstream has stated a checksum for. \
                 If '{host}' is a Git forge, configure it as a github/gitlab/forgejo \
                 registry and point the module at that registry's archive URL, which this \
                 proxy does cache and gate.",
                pkg.cache_key(),
                resolved.scheme()
            )));
        }

        self.stream_resolved(resolved, pkg, "module archive").await
    }
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for TerraformRegistryClient {
    fn registry_type(&self) -> &str {
        "terraform"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let url = self.artifact_url(pkg)?;

        let resp =
            self.get(&url).send().await.map_err(|e| {
                CoreError::Registry(format!("terraform metadata request failed: {e}"))
            })?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "terraform resource not found: {}",
                pkg.cache_key()
            )));
        }
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NO_CONTENT {
            return Err(CoreError::Registry(format!(
                "terraform metadata request returned {} for {}",
                resp.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&resp);

        // Fetch per-version publish timestamp for specific-version requests.
        // Version listings ("versions") have no meaningful single timestamp.
        let published_at = if pkg.version != "versions" {
            modules::fetch_version_published_at(self, pkg).await
        } else {
            None
        };

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url: Some(url),
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let url = self.artifact_url(pkg)?;

        tracing::debug!(url = %url, "fetching Terraform artifact");

        // A module's `/download` endpoint answers `204` with an
        // `X-Terraform-Get` header naming where the tarball actually lives — it
        // is a *pointer*, not the bytes. Streaming the pointer's empty body was
        // never useful; the handler used to forward the header instead, which
        // sent the client to fetch the tarball itself and took the download
        // clean out of this proxy's rule chain (RFC 0006 §13.6, RFC 0009 §7.2).
        //
        // Following it here is what puts those bytes back on the gated path.
        //
        // Only an actual module *download* — `version == "versions"` addresses
        // the version listing through this same method (see `artifact_url`),
        // and that document is fetched, not followed.
        if pkg.name.starts_with("modules/") && pkg.version != "versions" {
            return self.fetch_module_tarball(&url, pkg).await;
        }

        // The provider archive and its two sidecars are all named *inside* the
        // download document rather than addressed by a path, so each is fetched
        // by resolving that document and following one field.
        //
        // The archive used to be the exception: `artifact_url` returns the
        // download document's URL, and with no branch to follow it this method
        // streamed **the JSON document itself** as the provider zip — 8 KB of
        // metadata served as `application/zip`. Terraform 1.8.5 got as far as
        // *"archive has incorrect checksum"*, having verified the signature over
        // the checksums we had correctly proxied, against bytes that were not a
        // provider (RFC 0009 §12.12).
        if let Some(field) = match pkg.artifact.as_deref() {
            Some("shasums") => Some("shasums_url"),
            Some("shasums.sig") => Some("shasums_signature_url"),
            // `os/arch` — a platform, so the archive for it.
            Some(platform) if platform.contains('/') && pkg.name.starts_with("providers/") => {
                Some("download_url")
            }
            _ => None,
        } {
            return self.fetch_provider_sidecar(&url, field, pkg).await;
        }

        let response = self.get(&url).send().await.map_err(to_registry_error)?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "terraform artifact not found: {}",
                pkg.cache_key()
            )));
        }
        if !response.status().is_success() && response.status() != reqwest::StatusCode::NO_CONTENT {
            return Err(CoreError::Registry(format!(
                "terraform upstream returned {} for {}",
                response.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&response);

        let stream = response.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    /// One document kind covers both facets: `package` already carries the
    /// `modules/` or `providers/` prefix that decides the URL *and* the response
    /// shape, exactly as [`super::TerraformRegistryClient::artifact_url`] reads
    /// it. A `Secondary` kind here would be a second name for the same request.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if !package.starts_with("providers/") && !package.starts_with("modules/") {
            return Err(CoreError::Registry(format!(
                "terraform: invalid package name '{package}': must start with \
                 'providers/' or 'modules/'"
            )));
        }
        let base = self.base_url.trim_end_matches('/');

        // The download document is addressed in full by the package name, which
        // already ends in `{version}/download/{os}/{arch}` — the listing's
        // `/versions` suffix would name a different, and wrong, document.
        if kind == DocumentKind::PROVIDER_DOWNLOAD {
            if !package.starts_with("providers/") || !package.contains("/download/") {
                return Err(CoreError::Registry(format!(
                    "terraform: '{package}' does not address a provider download \
                     document (expected providers/{{ns}}/{{type}}/{{version}}/download/{{os}}/{{arch}})"
                )));
            }
            let url = format!("{base}/v1/{}", encode_path(package));
            return fetch_json_document(
                self.get(&url),
                &format!("terraform download document for '{package}'"),
            )
            .await;
        }

        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "terraform has no '{kind}' listing document"
            )));
        }
        let url = format!("{base}/v1/{}/versions", encode_path(package));
        fetch_json_document(
            self.get(&url),
            &format!("terraform versions for '{package}'"),
        )
        .await
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/v1/{}/versions", encode_path(package));

        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }

        let body = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .bytes()
            .await
            .map_err(to_registry_error)?;

        modules::parse_versions(package, &body)
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        let Some(ref base) = self.search_base else {
            return Ok(vec![]);
        };

        let per = limit.min(25);

        // 1. Full-text module search (registry protocol v1 — always works).
        // 2. Provider lookup strategy — the Terraform Registry Protocol has no
        //    full-text provider search. Uses two heuristics (namespace + exact).
        let mut results: Vec<UpstreamPackage> = Vec::new();

        results.extend(providers::search_modules(self, base, query, per).await);
        results.extend(providers::search_providers(self, base, query, per).await);

        // Deduplicate by name
        let mut seen = std::collections::HashSet::new();
        results.retain(|r| seen.insert(r.name.clone()));

        Ok(results)
    }
}
