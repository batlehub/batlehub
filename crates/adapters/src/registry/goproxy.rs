use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;
use serde::Deserialize;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
};

use super::http_client::{
    basic_auth_get, cache_control, encode_path, fetch_json_document, fetch_text_document,
    new_http_client, percent_encode, same_origin as urls_same_origin, to_registry_error,
    UpstreamHttpOptions,
};

use batlehub_core::ports::UpstreamPackage;

/// Go module proxy client (proxy.golang.org or compatible).
///
/// Implements the GOPROXY protocol: https://go.dev/ref/mod#goproxy-protocol
///
/// Supported `PackageId` conventions:
/// - `name` format: Go module path without trailing slash
///   (e.g. `"golang.org/x/text"`, `"github.com/!burnt!sushi/toml"`)
///   Uppercase-encoded paths (`!{lowercase}`) are passed through unchanged —
///   the upstream GOPROXY also expects the encoded form.
/// - `version = "latest"` → used for `@latest` and `@v/list` endpoints
/// - `version = "v1.2.3"` → specific version
/// - `artifact = None`         → stream `.info` JSON (or `@latest` JSON when version = "latest")
/// - `artifact = Some("list")` → stream `@v/list`
/// - `artifact = Some("mod")`  → stream `go.mod` file
/// - `artifact = Some("zip")`  → stream module source zip
///
/// **Caching note**: `@latest` and `@v/list` responses are cached permanently,
/// like all other artifacts. They may become stale after new module versions are
/// published. Clear the proxy storage to refresh, or pin versions in `go.sum`.
pub struct GoProxyRegistryClient {
    http: reqwest::Client,
    /// Credential-free client used for the search host. The default search target
    /// (pkg.go.dev) is a *different* host than the GOPROXY upstream, so the
    /// upstream's auth headers/basic-auth must never be sent to it. `None` when
    /// search is disabled (`search_url = ""`), so no client is allocated.
    search_http: Option<reqwest::Client>,
    base_url: String,
    /// Base URL for free-text search. The GOPROXY protocol has no search endpoint,
    /// so this points at a pkg.go.dev-compatible site (default `https://pkg.go.dev`).
    /// `None` disables upstream search (configured via `search_url = ""`).
    search_base: Option<String>,
    /// Whether search may reuse the credentialed upstream client — only true when
    /// the search host is the same origin as the GOPROXY upstream.
    search_authed: bool,
    basic_auth: Option<(String, String)>,
}

/// True when `a` and `b` share scheme, host and (effective) port.
fn same_origin(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a), reqwest::Url::parse(b)) {
        (Ok(x), Ok(y)) => urls_same_origin(&x, &y),
        _ => false,
    }
}

impl GoProxyRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;
        let base_url = base_url.into();
        // search_url: None → built-in default; Some("") → disabled; Some(u) → override.
        let search_base = match opts.search_url.as_deref() {
            None => Some("https://pkg.go.dev".to_owned()),
            Some("") => None,
            Some(u) => Some(u.trim_end_matches('/').to_owned()),
        };
        let search_authed = search_base
            .as_deref()
            .is_some_and(|s| same_origin(&base_url, s));
        // Only allocate the credential-free search client when search is enabled
        // and points at a different origin than the credentialed upstream.
        let search_http = if search_base.is_some() && !search_authed {
            Some(
                reqwest::Client::builder()
                    .user_agent("batlehub/0.1")
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .build()
                    .map_err(|e| CoreError::Other(e.into()))?,
            )
        } else {
            None
        };
        Ok(Self {
            http,
            search_http,
            base_url,
            search_base,
            search_authed,
            basic_auth: opts.basic_auth.clone(),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }
}

/// Parse module/package paths (and synopses) out of a pkg.go.dev search results
/// page. Best-effort and tolerant: unknown markup simply yields fewer results
/// rather than an error.
fn parse_pkg_go_dev_search(html: &str, limit: usize) -> Vec<UpstreamPackage> {
    let mut out: Vec<UpstreamPackage> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Each result lives under a `SearchSnippet-header`; the module/package path is
    // the first `href="/..."` inside it (the visible text contains <wbr> tags, so we
    // use the href, not the text).
    for chunk in html.split("SearchSnippet-header").skip(1) {
        if out.len() >= limit {
            break;
        }
        let Some(href_pos) = chunk.find("href=\"") else {
            continue;
        };
        let after = &chunk[href_pos + 6..];
        let Some(end) = after.find('"') else {
            continue;
        };
        let href = &after[..end];
        // `/github.com/foo/bar?tab=…` → `github.com/foo/bar`
        let path = href
            .trim_start_matches('/')
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if path.is_empty() || path.starts_with("search") || !path.contains('.') {
            continue;
        }
        if !seen.insert(path.to_owned()) {
            continue;
        }

        // Optional synopsis within this chunk.
        let description = chunk
            .find("SearchSnippet-synopsis")
            .and_then(|p| chunk[p..].find('>').map(|g| p + g + 1))
            .and_then(|start| {
                chunk[start..]
                    .find('<')
                    .map(|e| chunk[start..start + e].trim())
            })
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        out.push(UpstreamPackage {
            name: path.to_owned(),
            // pkg.go.dev search snippets don't carry a reliable version; the proxy
            // resolves @latest when the module is first fetched.
            latest_version: "latest".to_owned(),
            description,
        });
    }
    out
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GoVersionInfo {
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Time")]
    time: Option<String>,
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for GoProxyRegistryClient {
    fn registry_type(&self) -> &str {
        "goproxy"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let info = if pkg.version == "latest" {
            self.fetch_info_latest(&pkg.name).await?
        } else {
            self.fetch_info_version(&pkg.name, &pkg.version).await?
        };

        let published_at = info
            .time
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let extra = serde_json::json!({
            "resolved_version": info.version,
        });

        Ok(PackageMetadata {
            id: PackageId {
                version: info.version,
                ..pkg.clone()
            },
            published_at,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra,
            cache_control: None,
        })
    }

    /// `@v/list` is the version listing; `@latest` is a second document that
    /// names one version and carries no list.
    ///
    /// Repairing `@latest` needs the filtered `@v/list` as well, so it is not
    /// done here — see `blocking::goproxy::repaired_latest` and the handler that
    /// composes the two.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        match kind {
            DocumentKind::Versions => {
                let url = format!("{}/@v/list", self.module_base(package));
                fetch_text_document(
                    self.get(&url),
                    &format!("go @v/list for '{package}'"),
                    "text/plain; charset=utf-8",
                )
                .await
            }
            DocumentKind::LATEST => {
                let url = format!("{}/@latest", self.module_base(package));
                fetch_json_document(self.get(&url), &format!("go @latest for '{package}'")).await
            }
            other => Err(CoreError::NotSupported(format!(
                "go proxy has no '{other}' listing document"
            ))),
        }
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let url = format!("{}/@v/list", self.module_base(package));
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }

        let text = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .text()
            .await
            .map_err(to_registry_error)?;

        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect())
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let url = match pkg.artifact.as_deref() {
            Some("list") => {
                format!("{}/@v/list", self.module_base(&pkg.name))
            }
            Some("mod") => {
                format!(
                    "{}/@v/{}.mod",
                    self.module_base(&pkg.name),
                    encode_path(&pkg.version)
                )
            }
            Some("zip") => {
                format!(
                    "{}/@v/{}.zip",
                    self.module_base(&pkg.name),
                    encode_path(&pkg.version)
                )
            }
            None if pkg.version == "latest" => {
                format!("{}/@latest", self.module_base(&pkg.name))
            }
            None => {
                format!(
                    "{}/@v/{}.info",
                    self.module_base(&pkg.name),
                    encode_path(&pkg.version)
                )
            }
            Some(other) => {
                return Err(CoreError::Registry(format!(
                    "unknown goproxy artifact type '{other}'"
                )));
            }
        };

        tracing::debug!(url = %url, "fetching Go module artifact");

        let response = self.get(&url).send().await.map_err(to_registry_error)?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "Go module {}/{} not found",
                pkg.name, pkg.version
            )));
        }

        let response = response.error_for_status().map_err(to_registry_error)?;

        let cache_control = cache_control(&response);

        let stream = response.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        // The GOPROXY protocol has no search endpoint; query pkg.go.dev instead.
        let Some(base) = &self.search_base else {
            return Ok(vec![]);
        };
        let url = format!("{}/search?q={}&m=package", base, percent_encode(query));
        // Only attach upstream credentials when the search host is the same origin
        // as the GOPROXY upstream; otherwise use the credential-free client so we
        // never leak the upstream token/basic-auth to a third-party search site.
        let req = match (self.search_authed, self.search_http.as_ref()) {
            (true, _) => self.get(&url),
            (false, Some(client)) => client.get(&url),
            // search_http is built whenever search is enabled and cross-origin, so
            // this is unreachable past the `search_base` guard above; bail safely
            // rather than fall back to the credentialed client and leak auth.
            (false, None) => return Ok(vec![]),
        };
        let resp = match req.send().await {
            Ok(r) if r.status().is_success() => r,
            // Search is best-effort: a transport error or non-200 yields no results
            // rather than failing the whole multi-registry explore search.
            _ => return Ok(vec![]),
        };
        let html = resp.text().await.map_err(to_registry_error)?;
        Ok(parse_pkg_go_dev_search(&html, limit))
    }
}

impl GoProxyRegistryClient {
    /// Returns `{base_url}/{module}` — the common prefix for all GOPROXY endpoints.
    fn module_base(&self, module: &str) -> String {
        format!("{}/{}", self.base_url, encode_path(module))
    }

    async fn fetch_info_latest(&self, module: &str) -> Result<GoVersionInfo, CoreError> {
        let url = format!("{}/@latest", self.module_base(module));
        self.fetch_info_url(&url, module, "latest").await
    }

    async fn fetch_info_version(
        &self,
        module: &str,
        version: &str,
    ) -> Result<GoVersionInfo, CoreError> {
        let url = format!(
            "{}/@v/{}.info",
            self.module_base(module),
            encode_path(version)
        );
        self.fetch_info_url(&url, module, version).await
    }

    async fn fetch_info_url(
        &self,
        url: &str,
        module: &str,
        version: &str,
    ) -> Result<GoVersionInfo, CoreError> {
        let resp = self.get(url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "Go module {module}@{version} not found"
            )));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<GoVersionInfo>()
            .await
            .map_err(to_registry_error)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use mockito::Server;

    fn pkg(name: &str, version: &str) -> PackageId {
        PackageId::new("go", name, version)
    }

    #[tokio::test]
    async fn resolve_metadata_latest() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/golang.org/x/text/@latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Version":"v0.14.0","Time":"2023-11-09T18:25:45Z"}"#)
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("golang.org/x/text", "latest"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "v0.14.0");
        assert!(meta.published_at.is_some());
    }

    #[tokio::test]
    async fn resolve_metadata_specific_version() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/golang.org/x/text/@v/v0.3.7.info")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Version":"v0.3.7","Time":"2021-06-17T00:00:00Z"}"#)
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("golang.org/x/text", "v0.3.7"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "v0.3.7");
        assert!(meta.published_at.is_some());
    }

    #[tokio::test]
    async fn resolve_metadata_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/example.com/unknown/@latest")
            .with_status(404)
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .resolve_metadata(&pkg("example.com/unknown", "latest"))
            .await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_info() {
        let mut server = Server::new_async().await;
        let body = r#"{"Version":"v0.3.7","Time":"2021-06-17T00:00:00Z"}"#;
        let _mock = server
            .mock("GET", "/golang.org/x/text/@v/v0.3.7.info")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let fetched = client
            .fetch_artifact(&pkg("golang.org/x/text", "v0.3.7"))
            .await
            .unwrap();
        let bytes: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        let content = bytes
            .into_iter()
            .flat_map(|b| b.to_vec())
            .collect::<Vec<u8>>();
        let content = String::from_utf8(content).unwrap();
        assert!(content.contains("v0.3.7"));
    }

    // ── list_versions ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_versions_returns_versions_from_list_endpoint() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/golang.org/x/text/@v/list")
            .with_status(200)
            .with_body("v0.3.6\nv0.3.7\nv0.14.0\n")
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let versions = client.list_versions("golang.org/x/text").await.unwrap();

        assert_eq!(versions, vec!["v0.3.6", "v0.3.7", "v0.14.0"]);
    }

    #[tokio::test]
    async fn list_versions_returns_empty_when_module_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/example.com/unknown/@v/list")
            .with_status(404)
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let versions = client.list_versions("example.com/unknown").await.unwrap();

        assert!(versions.is_empty());
    }

    #[tokio::test]
    async fn list_versions_ignores_blank_lines() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/example.com/mod/@v/list")
            .with_status(200)
            .with_body("\nv1.0.0\n\nv1.1.0\n")
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let versions = client.list_versions("example.com/mod").await.unwrap();

        assert_eq!(versions, vec!["v1.0.0", "v1.1.0"]);
    }

    #[tokio::test]
    async fn fetch_artifact_list() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/golang.org/x/text/@v/list")
            .with_status(200)
            .with_body("v0.3.6\nv0.3.7\n")
            .create_async()
            .await;

        let client = GoProxyRegistryClient::new(server.url(), &Default::default()).unwrap();
        let pkg_list = pkg("golang.org/x/text", "latest").with_artifact("list");
        let fetched = client.fetch_artifact(&pkg_list).await.unwrap();
        let bytes: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        let content = bytes
            .into_iter()
            .flat_map(|b| b.to_vec())
            .collect::<Vec<u8>>();
        let content = String::from_utf8(content).unwrap();
        assert!(content.contains("v0.3.7"));
    }

    const SEARCH_HTML: &str = r#"
      <div class="SearchSnippet">
        <h2 class="SearchSnippet-header">
          <a href="/github.com/spf13/cobra?tab=overview" data-gtmc="search result">github.com/spf13/<wbr>cobra</a>
        </h2>
        <p class="SearchSnippet-synopsis" data-test-id="snippet-synopsis">A Commander for modern Go CLI interactions</p>
      </div>
      <div class="SearchSnippet">
        <h2 class="SearchSnippet-header">
          <a href="/golang.org/x/text" data-gtmc="search result">golang.org/x/<wbr>text</a>
        </h2>
        <p class="SearchSnippet-synopsis">Go text processing support</p>
      </div>
    "#;

    #[test]
    fn parse_search_extracts_paths_and_synopsis() {
        let results = parse_pkg_go_dev_search(SEARCH_HTML, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "github.com/spf13/cobra");
        assert_eq!(
            results[0].description.as_deref(),
            Some("A Commander for modern Go CLI interactions")
        );
        assert_eq!(results[0].latest_version, "latest");
        assert_eq!(results[1].name, "golang.org/x/text");
    }

    #[test]
    fn parse_search_respects_limit_and_dedups() {
        assert_eq!(parse_pkg_go_dev_search(SEARCH_HTML, 1).len(), 1);
        let dup = format!("{SEARCH_HTML}{SEARCH_HTML}");
        assert_eq!(parse_pkg_go_dev_search(&dup, 10).len(), 2);
    }

    #[tokio::test]
    async fn search_packages_queries_pkg_go_dev() {
        let mut server = Server::new_async().await;
        let m = server
            .mock("GET", "/search?q=cobra&m=package")
            .with_status(200)
            .with_body(SEARCH_HTML)
            .create_async()
            .await;
        let opts = UpstreamHttpOptions {
            search_url: Some(server.url()),
            ..Default::default()
        };
        let client = GoProxyRegistryClient::new("https://proxy.golang.org", &opts).unwrap();
        let results = client.search_packages("cobra", 10).await.unwrap();
        m.assert_async().await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "github.com/spf13/cobra");
    }

    #[tokio::test]
    async fn search_disabled_returns_empty() {
        let opts = UpstreamHttpOptions {
            search_url: Some(String::new()), // search_url = "" disables search
            ..Default::default()
        };
        let client = GoProxyRegistryClient::new("https://proxy.golang.org", &opts).unwrap();
        assert!(client
            .search_packages("cobra", 10)
            .await
            .unwrap()
            .is_empty());
    }
}
