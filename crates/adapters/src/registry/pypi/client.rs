use async_trait::async_trait;
use futures::TryStreamExt;

use super::super::http_client::{
    cache_control, fetch_document_bytes, fetch_text_document, to_registry_error,
};
use super::models::{PypiPackageJson, PypiSearchInfo, PypiVersionJson};
use super::PypiRegistryClient;
use batlehub_core::{
    entities::{MetadataLinks, MetadataReadme, PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
    services::readme::detect::format_from_content_type,
};

// ── PEP 503 name normalisation ────────────────────────────────────────────────

/// The PEP 691 media type, sent as `Accept` and echoed as the response's
/// `Content-Type`. Pinned to `v1` rather than the unversioned
/// `application/vnd.pypi.simple+json`, which servers may answer with a newer
/// schema this proxy has not been taught to filter.
pub const SIMPLE_JSON_ACCEPT: &str = "application/vnd.pypi.simple.v1+json";

/// Normalise a PyPI package name per PEP 503: lower-case, collapse runs of
/// `[-_.]` into a single `-`.
///
/// Delegates to `RegistryKind::canonical_package_name`, which is where the rule
/// is defined once: the explore catalogue compares a search hit's display
/// spelling against a stored name through that same function, and a second
/// definition here could drift from it.
pub fn normalize_name(name: &str) -> String {
    batlehub_core::entities::RegistryKind::Pypi
        .canonical_package_name(name)
        .into_owned()
}

/// Fetch the Simple API HTML (or JSON) page for a package from the upstream.
///
/// Returns the raw body bytes and the `Content-Type` header value so the
/// handler can forward it to the client after URL rewriting.
pub async fn fetch_simple_page(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
    basic_auth: Option<&(String, String)>,
    accept: Option<&str>,
) -> Result<(bytes::Bytes, Option<String>), CoreError> {
    let normalized = normalize_name(name);
    let url = format!("{}/simple/{}/", base_url.trim_end_matches('/'), normalized);

    let mut builder = client.get(&url);
    if let Some((u, p)) = basic_auth {
        builder = builder.basic_auth(u, Some(p));
    }
    if let Some(accept_val) = accept {
        builder = builder.header(reqwest::header::ACCEPT, accept_val);
    }

    let resp = builder
        .send()
        .await
        .map_err(|e| CoreError::Registry(format!("pypi: simple page request failed: {e}")))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(CoreError::NotFound(format!(
            "pypi: package '{}' not found in simple index",
            name
        )));
    }
    if !resp.status().is_success() {
        return Err(CoreError::Registry(format!(
            "pypi: simple index returned {}",
            resp.status()
        )));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let body = resp.bytes().await.map_err(to_registry_error)?;

    Ok((body, content_type))
}

/// Rewrite href/url values in a PyPI simple page so all file links go through
/// this registry's `packages/{filename}` endpoint.
///
/// `registry_base` is the registry's public base URL as seen by the requesting
/// client — `https://pypi.acme.io` on a host-routed request,
/// `https://hub.example.com/proxy/pypi1` on the subpath. The shape of the ingress
/// is the caller's business; this function only appends `/packages/{filename}`.
///
/// Handles both HTML (PEP 503) and JSON (PEP 691) formats.
pub fn rewrite_simple_page(
    body: &[u8],
    content_type: Option<&str>,
    registry_base: &str,
) -> Vec<u8> {
    let is_json = content_type
        .map(|ct| ct.contains("application/vnd.pypi.simple"))
        .unwrap_or(false);

    if is_json {
        rewrite_simple_json(body, registry_base)
    } else {
        rewrite_simple_html(body, registry_base)
    }
}

/// Rewrite one `href` value if it is an absolute HTTP URL pointing to a PyPI
/// CDN file. Returns `Some(rewritten)` when rewriting is applicable, `None`
/// when the original value should be kept unchanged.
fn rewrite_abs_href(href_value: &str, proxy_packages: &str) -> Option<String> {
    if !href_value.starts_with("https://") && !href_value.starts_with("http://") {
        return None;
    }
    if let Some(fragment_pos) = href_value.rfind('#') {
        let path_part = &href_value[..fragment_pos];
        let fragment = &href_value[fragment_pos..];
        if let Some(slash_pos) = path_part.rfind('/') {
            let filename = &path_part[slash_pos + 1..];
            return Some(format!("{proxy_packages}/{filename}{fragment}"));
        }
    } else if let Some(slash_pos) = href_value.rfind('/') {
        let filename = &href_value[slash_pos + 1..];
        return Some(format!("{proxy_packages}/{filename}"));
    }
    None
}

pub(super) fn rewrite_simple_html(body: &[u8], registry_base: &str) -> Vec<u8> {
    let text = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return body.to_vec(),
    };

    let proxy_packages = format!("{registry_base}/packages");
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(href_pos) = remaining.find("href=\"") {
        let after_quote = &remaining[href_pos + 6..];
        result.push_str(&remaining[..href_pos + 6]);

        if let Some(end_quote) = after_quote.find('"') {
            let href_value = &after_quote[..end_quote];
            remaining = &after_quote[end_quote..];
            let rewritten = rewrite_abs_href(href_value, &proxy_packages)
                .unwrap_or_else(|| href_value.to_owned());
            result.push_str(&rewritten);
        } else {
            remaining = after_quote;
        }
    }
    result.push_str(remaining);
    result.into_bytes()
}

pub(super) fn rewrite_simple_json(body: &[u8], registry_base: &str) -> Vec<u8> {
    let mut json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return body.to_vec(),
    };

    let proxy_packages = format!("{registry_base}/packages");

    if let Some(files) = json.get_mut("files").and_then(|f| f.as_array_mut()) {
        for file in files.iter_mut() {
            if let Some(url_val) = file.get_mut("url") {
                if let Some(url_str) = url_val.as_str() {
                    let rewritten = rewrite_file_url(url_str, &proxy_packages);
                    *url_val = serde_json::Value::String(rewritten);
                }
            }
        }
    }

    serde_json::to_vec(&json).unwrap_or_else(|_| body.to_vec())
}

fn rewrite_file_url(url: &str, proxy_packages: &str) -> String {
    // Split off fragment first
    let (path_part, fragment) = if let Some(frag_pos) = url.rfind('#') {
        (&url[..frag_pos], &url[frag_pos..])
    } else {
        (url, "")
    };

    if let Some(slash_pos) = path_part.rfind('/') {
        let filename = &path_part[slash_pos + 1..];
        format!("{proxy_packages}/{filename}{fragment}")
    } else {
        url.to_owned()
    }
}

/// What pypi.org's JSON API said about a file, kept apart from "the API is
/// not there": only the latter sends the lookup to the simple page.
enum JsonApiAnswer {
    /// No `/pypi/{name}/{version}/json` upstream (`404`) — a PEP 503-only index.
    NoApi,
    /// The API knows the release and lists the file.
    File(String),
    /// The API knows the release; the file is not in it.
    NoSuchFile,
}

impl PypiRegistryClient {
    /// The file's URL from pypi.org's JSON API — see [`JsonApiAnswer`].
    async fn file_url_from_json_api(
        &self,
        base: &str,
        name: &str,
        version: &str,
        filename: &str,
    ) -> Result<JsonApiAnswer, CoreError> {
        let api_url = format!("{base}/pypi/{name}/{version}/json");
        let api_resp = self
            .get(&api_url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("pypi: API request failed: {e}")))?;
        if api_resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(JsonApiAnswer::NoApi);
        }
        if !api_resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "pypi upstream returned {} for {name} {version}",
                api_resp.status()
            )));
        }
        let body = api_resp.bytes().await.map_err(to_registry_error)?;
        let version_json: PypiVersionJson = serde_json::from_slice(&body)
            .map_err(|e| CoreError::Registry(format!("pypi: parse version JSON: {e}")))?;
        Ok(version_json
            .urls
            .into_iter()
            .find(|f| f.filename == filename)
            .map_or(JsonApiAnswer::NoSuchFile, |f| JsonApiAnswer::File(f.url)))
    }

    /// Every file the simple page names — PEP 691 JSON or PEP 503 HTML,
    /// whichever the upstream speaks — with URLs resolved against the page.
    async fn files_from_simple_page(
        &self,
        base: &str,
        name: &str,
    ) -> Result<Vec<(String, String)>, CoreError> {
        let page_url = format!("{base}/simple/{name}/");
        let (body, content_type) = fetch_simple_page(
            &self.http,
            base,
            name,
            self.basic_auth.as_ref(),
            Some(&format!("{SIMPLE_JSON_ACCEPT}, text/html;q=0.1")),
        )
        .await?;
        Ok(files_in_simple_page(
            &page_url,
            &body,
            content_type.as_deref(),
        ))
    }

    async fn file_url_from_simple_page(
        &self,
        base: &str,
        name: &str,
        filename: &str,
    ) -> Result<Option<String>, CoreError> {
        Ok(self
            .files_from_simple_page(base, name)
            .await?
            .into_iter()
            .find(|(n, _)| n == filename)
            .map(|(_, url)| url))
    }

    /// [`RegistryClient::resolve_metadata`] for an upstream with no JSON API:
    /// what a PEP 503 page can say about one file — its URL and the sha256 in
    /// the link's fragment — and nothing it cannot (no upload time, no
    /// description, no links). The file is `pkg.artifact` when the request
    /// names one, else any file of this version, recognised by the
    /// `{name}-{version}` prefix both wheel and sdist names carry.
    async fn metadata_from_simple_page(
        &self,
        base: &str,
        name: &str,
        pkg: &PackageId,
    ) -> Result<PackageMetadata, CoreError> {
        let files = self.files_from_simple_page(base, name).await?;
        let underscored = name.replace('-', "_");
        let file = match pkg.artifact.as_deref() {
            Some(filename) => files.into_iter().find(|(n, _)| n == filename),
            None => files.into_iter().find(|(n, _)| {
                let lower = n.to_ascii_lowercase();
                [&underscored, name].iter().any(|stem| {
                    lower.starts_with(&format!("{stem}-{}", pkg.version.to_ascii_lowercase()))
                })
            }),
        };
        let Some((_, url)) = file else {
            return Err(CoreError::NotFound(format!(
                "pypi package not found: {} (no JSON API upstream, and the simple page names no such file)",
                pkg.cache_key()
            )));
        };
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: None,
            checksum: sha256_fragment(&url),
            download_url: Some(url),
            is_signed: None,
            extra: serde_json::json!({ "readme": null, "links": null }),
            cache_control: None,
        })
    }
}

/// Every file a simple page names, as `(filename, absolute url)`. A JSON
/// page names files outright; an HTML page is scanned for `href="…"` and the
/// filename is the last path segment before any `#fragment` — the only part
/// of a PEP 503 link that is specified. Relative hrefs (a static mirror's
/// `../../packages/x.whl`) are resolved against the page's URL, which is what
/// pip does with them; the fragment (`#sha256=…`) is kept on the URL.
pub fn files_in_simple_page(
    page_url: &str,
    body: &[u8],
    content_type: Option<&str>,
) -> Vec<(String, String)> {
    let Ok(page) = reqwest::Url::parse(page_url) else {
        return Vec::new();
    };
    let absolute = |href: &str| page.join(href).ok().map(|u| u.to_string());
    if content_type.is_some_and(|ct| ct.contains("application/vnd.pypi.simple")) {
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(body) else {
            return Vec::new();
        };
        return doc
            .get("files")
            .and_then(|f| f.as_array())
            .map(|files| {
                files
                    .iter()
                    .filter_map(|f| {
                        let name = f.get("filename")?.as_str()?;
                        let url = absolute(f.get("url")?.as_str()?)?;
                        Some((name.to_owned(), url))
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find("href=\"") {
        let after = &rest[pos + 6..];
        let Some(end) = after.find('"') else { break };
        let href = &after[..end];
        rest = &after[end..];
        let path = href.split(['#', '?']).next().unwrap_or(href);
        if let (Some(name), Some(url)) = (path.rsplit('/').next(), absolute(href)) {
            if !name.is_empty() {
                out.push((name.to_owned(), url));
            }
        }
    }
    out
}

/// The `sha256=` a PEP 503 link carries in its fragment, if any.
fn sha256_fragment(url: &str) -> Option<String> {
    url.rsplit_once('#')
        .map(|(_, fragment)| fragment)
        .and_then(|fragment| fragment.strip_prefix("sha256="))
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for PypiRegistryClient {
    fn registry_type(&self) -> &str {
        "pypi"
    }

    /// A simple-index page, in whichever of its two representations the caller
    /// asked for.
    ///
    /// PEP 503 HTML and PEP 691 JSON are different bytes for the same URL, so
    /// they are different `DocumentKind`s — keyed together in the metadata
    /// cache, whichever one warmed the entry would be served to clients that
    /// asked for the other. The `Accept` this sends upstream is derived from the
    /// kind for the same reason.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let name = normalize_name(package);
        let url = format!("{base}/simple/{name}/");
        let what = format!("pypi simple page for '{package}'");

        match kind {
            // JSON asked for, HTML accepted: PEP 691 is a content negotiation,
            // and an index that only speaks PEP 503 — a static directory, an
            // older Nexus, devpi — answers `text/html` to this `Accept`. pip
            // lists HTML in its own `Accept` (at a lower q) for exactly that
            // case, so the honest answer is the HTML page as HTML, not a `502`
            // for "expected value at line 1" (measured by tests/heavy/backends.sh
            // against a served directory). The upstream's `Content-Type`, not
            // the kind, decides how the body is read.
            DocumentKind::SIMPLE_JSON => {
                let req = self.get(&url).header(
                    reqwest::header::ACCEPT,
                    format!("{SIMPLE_JSON_ACCEPT}, text/html;q=0.1"),
                );
                let (body, content_type) = fetch_document_bytes(req, &what).await?;
                let is_json = content_type
                    .as_deref()
                    .is_some_and(|ct| ct.contains("application/vnd.pypi.simple"));
                if is_json {
                    let value: serde_json::Value = serde_json::from_slice(&body)
                        .map_err(|e| CoreError::Registry(format!("parsing {what}: {e}")))?;
                    let mut doc = VersionDocument::json(value);
                    doc.content_type = SIMPLE_JSON_ACCEPT.to_owned();
                    Ok(doc)
                } else {
                    let text = String::from_utf8(body.to_vec()).map_err(|e| {
                        CoreError::Registry(format!("{what} is not valid UTF-8: {e}"))
                    })?;
                    Ok(VersionDocument::text("text/html; charset=utf-8", text))
                }
            }
            DocumentKind::Versions => {
                let req = self.get(&url).header(reqwest::header::ACCEPT, "text/html");
                fetch_text_document(req, &what, "text/html; charset=utf-8").await
            }
            other => Err(CoreError::NotSupported(format!(
                "pypi has no '{other}' listing document"
            ))),
        }
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let name = normalize_name(&pkg.name);
        let url = format!("{base}/pypi/{name}/{}/json", pkg.version);

        let resp = self
            .get(&url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("pypi metadata request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            // No JSON API, or no such release: the simple page is the
            // document that decides, the way it does for `fetch_artifact`.
            return self.metadata_from_simple_page(base, &name, pkg).await;
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "pypi upstream returned {} for {}",
                resp.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&resp);

        let body = resp.bytes().await.map_err(to_registry_error)?;

        let version_json: PypiVersionJson = serde_json::from_slice(&body)
            .map_err(|e| CoreError::Registry(format!("pypi: parse version JSON: {e}")))?;

        // The long description, before `urls` is consumed below. Metadata-borne
        // and per-version: a wheel's `METADATA` ships inside the wheel, so this
        // is this version's own account of itself and not the package's.
        let readme = version_json.info.as_ref().and_then(|info| {
            let text = info
                .description
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())?;
            Some(MetadataReadme::text(
                text,
                format_from_content_type(info.description_content_type.as_deref()),
            ))
        });

        // Also before `info` goes out of scope with the rest of the document.
        let links = version_json
            .info
            .as_ref()
            .and_then(|info| MetadataLinks::new(info.repository(), info.homepage()));

        // Find the specific file matching pkg.artifact, or use the first file.
        let file = match &pkg.artifact {
            Some(filename) => version_json
                .urls
                .into_iter()
                .find(|f| f.filename == *filename),
            None => version_json.urls.into_iter().next(),
        };

        let (download_url, checksum, published_at) = match file {
            Some(f) => {
                let published_at = f.upload_time_iso_8601.as_deref().and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                });
                (Some(f.url), f.digests.sha256, published_at)
            }
            None => (None, None, None),
        };

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url,
            checksum,
            is_signed: None,
            extra: serde_json::json!({ "readme": readme, "links": links }),
            cache_control,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let name = normalize_name(&pkg.name);
        let version = &pkg.version;

        let artifact_filename = pkg.artifact.as_deref().unwrap_or("");

        // PEP 658: pip and uv resolve from `{file}.metadata` rather than
        // downloading the wheel, and the simple page we serve advertises it
        // (`data-core-metadata` survives the href rewrite). Not answering it is
        // not a slow path — pip **fails the install** on the 404 rather than
        // falling back (RFC 0009 §12.7). So the sibling is resolved from the
        // same file entry, with the suffix carried onto the CDN URL.
        let (match_name, metadata_sibling) = match artifact_filename.strip_suffix(".metadata") {
            Some(base) => (base, true),
            None => (artifact_filename, false),
        };

        // Resolve the download URL: the JSON API first, then the simple page.
        // The JSON API (`/pypi/{name}/{version}/json`) is pypi.org's own and
        // nothing in PEP 503 or PEP 691 requires it; a static mirror, devpi
        // or a Nexus answer `404` there and carry every file link on the
        // simple page instead — which is also the document this proxy already
        // served the client to get here (measured by tests/heavy/backends.sh
        // against a served directory: the page came through, the wheel was a
        // `404`). The page's own URL resolves a relative href.
        let file_url = match self
            .file_url_from_json_api(base, &name, version, match_name)
            .await?
        {
            JsonApiAnswer::File(url) => url,
            // The API knows the release and does not list the file: it does
            // not exist, and the simple page would only say so again.
            JsonApiAnswer::NoSuchFile => {
                return Err(CoreError::NotFound(format!(
                    "pypi: file '{match_name}' not found in version {version}"
                )));
            }
            JsonApiAnswer::NoApi => self
                .file_url_from_simple_page(base, &name, match_name)
                .await?
                .ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "pypi: file '{match_name}' not found in version {version} (no JSON API upstream, and the simple page does not name it)"
                    ))
                })?,
        };

        let download_url = if metadata_sibling {
            format!("{file_url}.metadata")
        } else {
            file_url
        };

        tracing::debug!(url = %download_url, "fetching PyPI artifact");

        // SSRF-guarded: validates the URL (and every redirect hop) against
        // private/reserved addresses and scopes credentials to the index origin.
        let dl_resp = self.download(&download_url).await?;

        if !dl_resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "pypi CDN returned {} for {}",
                dl_resp.status(),
                artifact_filename
            )));
        }

        let cache_control = cache_control(&dl_resp);

        let stream = dl_resp.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let name = normalize_name(package);
        let url = format!("{base}/pypi/{name}/json");

        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "pypi upstream returned {} listing versions for {name}",
                resp.status()
            )));
        }

        let body = resp.bytes().await.map_err(to_registry_error)?;

        let pkg_json: PypiPackageJson = serde_json::from_slice(&body)
            .map_err(|e| CoreError::Registry(format!("pypi: parse package JSON: {e}")))?;

        let mut versions: Vec<String> = pkg_json.releases.into_keys().collect();
        versions.sort();
        Ok(versions)
    }

    // PyPI removed its public search XMLRPC endpoint. Fall back to exact name
    // lookup: if the query exactly matches a published package, return it.
    async fn search_packages(
        &self,
        query: &str,
        _limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/pypi/{}/json", normalize_name(query));
        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: PypiSearchInfo = res.json().await.map_err(to_registry_error)?;

        Ok(vec![UpstreamPackage {
            name: body.info.name,
            latest_version: body.info.version,
            description: body.info.summary,
        }])
    }
}
