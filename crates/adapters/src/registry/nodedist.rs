//! The `nodejs.org/dist` file tree as a typed registry (RFC 0010).
//!
//! The tree has no metadata API. What it has is one listing — `index.tab`,
//! with `index.json` as the same rows in JSON — and one directory per release
//! holding a tarball per platform, `SHASUMS256.txt` and its detached
//! signatures. This client reads the listing for everything the rule engine
//! asks about a release, and streams the files as they are.
//!
//! Coordinates (RFC 0010 §4.3): one package, `node` (`iojs` on an io.js
//! tree), one version per release directory (`v22.11.0`, the `v` included —
//! it is what the tree and nvm both spell), and the file name as the artifact:
//!
//! ```text
//! PackageId { name: "node", version: "v22.11.0", artifact: Some("node-v22.11.0-linux-x64.tar.xz") }
//! → GET {base}/v22.11.0/node-v22.11.0-linux-x64.tar.xz
//! ```
//!
//! `published_at` comes from column two of `index.tab` (§6.4): the listing is
//! fetched once and kept for [`INDEX_TTL`], so the age gate works for Node
//! without a second request per file. A release the index no longer lists is
//! confirmed with a `HEAD` on the file and reaches the gate with no date —
//! which is the case `deny_missing_timestamp` exists to decide, and why config
//! validation makes an operator state it on this kind.
//!
//! No redirect chain: the tree serves its own bytes. `SHASUMS256.txt` is never
//! read here and never rewritten anywhere (§7).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::nodedist::{index_tab_versions, parse_index_date, IndexTab},
};

use super::http_client::{
    basic_auth_get, cache_control, fetch_json_document, fetch_text_document, new_http_client,
    to_registry_error, UpstreamHttpOptions,
};

/// How long one fetched `index.tab` answers `resolve_metadata` before it is
/// re-read.
///
/// Short enough that a release published today gets its date within the hour
/// the age gate cares about, long enough that a `nvm install` fan-out across
/// a build farm reads the listing once rather than once per file. The
/// *proxied* listing has its own cache in `ProxyService`, keyed by the
/// registry's `metadata_ttl`; this one is only for the dates.
const INDEX_TTL: Duration = Duration::from_secs(300);

/// `Cache-Control` for the streamed files, when upstream sends one.
struct CachedIndex {
    fetched_at: Instant,
    /// `version` → `(date, lts codename)`, as `index.tab` spells them.
    rows: HashMap<String, (String, Option<String>)>,
}

pub struct NodeDistRegistryClient {
    http: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
    index: Mutex<Option<CachedIndex>>,
}

impl NodeDistRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(5), opts)?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            basic_auth: opts.basic_auth.clone(),
            index: Mutex::new(None),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    fn head(&self, url: &str) -> reqwest::RequestBuilder {
        let rb = self.http.head(url);
        match &self.basic_auth {
            Some((u, p)) => rb.basic_auth(u, Some(p)),
            None => rb,
        }
    }

    fn file_url(&self, version: &str, file: &str) -> String {
        format!("{}/{version}/{file}", self.base_url)
    }

    /// The index rows, from the in-process copy when it is fresh.
    async fn index_rows(&self) -> Result<HashMap<String, (String, Option<String>)>, CoreError> {
        if let Some(cached) = self.index.lock().expect("index lock").as_ref() {
            if cached.fetched_at.elapsed() < INDEX_TTL {
                return Ok(cached.rows.clone());
            }
        }
        let text = self.fetch_index_tab().await?;
        let rows: HashMap<String, (String, Option<String>)> = IndexTab::parse(&text)
            .map(|tab| {
                tab.rows()
                    .map(|r| {
                        (
                            r.version.to_owned(),
                            (r.date.to_owned(), r.lts.map(str::to_owned)),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        *self.index.lock().expect("index lock") = Some(CachedIndex {
            fetched_at: Instant::now(),
            rows: rows.clone(),
        });
        Ok(rows)
    }

    async fn fetch_index_tab(&self) -> Result<String, CoreError> {
        let url = format!("{}/index.tab", self.base_url);
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        resp.error_for_status()
            .map_err(to_registry_error)?
            .text()
            .await
            .map_err(to_registry_error)
    }

    /// Whether `{version}/{file}` exists upstream, for a release the index no
    /// longer lists. A `HEAD`, so a 200 MB tarball is not read to answer a
    /// yes/no question.
    async fn file_exists(&self, version: &str, file: &str) -> Result<bool, CoreError> {
        let resp = self
            .head(&self.file_url(version, file))
            .send()
            .await
            .map_err(to_registry_error)?;
        match resp.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s if s.is_success() => Ok(true),
            s => Err(CoreError::Registry(format!(
                "HEAD {}/{version}/{file} returned {s} from upstream",
                self.base_url
            ))),
        }
    }
}

#[async_trait]
impl RegistryClient for NodeDistRegistryClient {
    fn registry_type(&self) -> &str {
        "nodedist"
    }

    /// The release date out of `index.tab`, and a `HEAD` on the file for a
    /// release the index no longer lists.
    ///
    /// A lookup that fails to *read the index* is not a missing release: it is
    /// logged and the file is checked directly, so an upstream hiccup on the
    /// listing degrades to "no date" — which the gate then treats exactly as
    /// the operator told it to — rather than to a `404` for a release that
    /// exists.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let version = pkg.version.as_str();
        let rows = match self.index_rows().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(
                    base = %self.base_url,
                    error = %e,
                    "could not read index.tab; resolving the release without a date"
                );
                HashMap::new()
            }
        };

        let (published_at, extra) = match rows.get(version) {
            Some((date, lts)) => (
                parse_index_date(date),
                serde_json::json!({ "date": date, "lts": lts }),
            ),
            None => {
                // Not listed: confirm the bytes exist before the rule engine
                // judges a release that may simply be a typo.
                let file = pkg.artifact.as_deref().unwrap_or("SHASUMS256.txt");
                if !self.file_exists(version, file).await? {
                    return Err(CoreError::NotFound(format!(
                        "Node release {version} ({file}) not found upstream"
                    )));
                }
                (None, serde_json::json!({ "date": null, "lts": null }))
            }
        };

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra,
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let Some(file) = pkg.artifact.as_deref() else {
            return Err(CoreError::NotFound(format!(
                "a Node release is a set of files; name one under {}",
                pkg.version
            )));
        };
        let url = self.file_url(&pkg.version, file);
        tracing::debug!(url = %url, "fetching Node dist file");

        let response = self.get(&url).send().await.map_err(to_registry_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "Node release {}/{file} not found upstream",
                pkg.version
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

    /// `index.tab` as text and `index.json` as JSON. `package` is unused: the
    /// tree has exactly one, and both documents describe it.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        match kind {
            DocumentKind::Versions => {
                let url = format!("{}/index.tab", self.base_url);
                fetch_text_document(
                    self.get(&url),
                    &format!("index.tab for '{package}'"),
                    "text/plain; charset=utf-8",
                )
                .await
            }
            DocumentKind::INDEX_JSON => {
                let url = format!("{}/index.json", self.base_url);
                fetch_json_document(self.get(&url), &format!("index.json for '{package}'")).await
            }
            other => Err(CoreError::NotSupported(format!(
                "the Node dist tree has no '{other}' listing document"
            ))),
        }
    }

    /// Every release in `index.tab`, oldest first as the trait asks — the
    /// tree writes them newest first.
    async fn list_versions(&self, _package: &str) -> Result<Vec<String>, CoreError> {
        let mut versions = index_tab_versions(&self.fetch_index_tab().await?);
        versions.reverse();
        Ok(versions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    const INDEX_TAB: &str = "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
        v22.11.0\t2024-10-29\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t1.3.0.1-motley\t3.0.15+quic\t127\tJod\t-\n\
        v22.10.0\t2024-10-16\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t1.3.0.1-motley\t3.0.15+quic\t127\t-\t-\n";

    const IOJS_INDEX_TAB: &str = "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\n\
        v3.3.1\t2015-09-15\theaders,linux-x64,src\t2.14.3\t4.4.63.30\t1.7.4\t1.2.8\t1.0.2d\t45\n";

    fn client(server: &Server) -> NodeDistRegistryClient {
        NodeDistRegistryClient::new(server.url(), &Default::default()).unwrap()
    }

    fn pkg(version: &str, file: &str) -> PackageId {
        PackageId::new("node", "node", version).with_artifact(file)
    }

    #[tokio::test]
    async fn resolve_metadata_reads_the_release_date_from_index_tab() {
        let mut server = Server::new_async().await;
        let _index = server
            .mock("GET", "/index.tab")
            .with_body(INDEX_TAB)
            .create_async()
            .await;

        let meta = client(&server)
            .resolve_metadata(&pkg("v22.11.0", "node-v22.11.0-linux-x64.tar.xz"))
            .await
            .unwrap();

        assert_eq!(
            meta.published_at.map(|d| d.to_rfc3339()),
            Some("2024-10-29T00:00:00+00:00".to_owned())
        );
        assert_eq!(meta.extra["lts"], "Jod");
    }

    /// One `index.tab` fetch answers every file of every release for a while:
    /// the second resolve makes no request at all.
    #[tokio::test]
    async fn the_index_is_fetched_once_for_many_resolves() {
        let mut server = Server::new_async().await;
        let index = server
            .mock("GET", "/index.tab")
            .with_body(INDEX_TAB)
            .expect(1)
            .create_async()
            .await;
        let c = client(&server);

        c.resolve_metadata(&pkg("v22.11.0", "SHASUMS256.txt"))
            .await
            .unwrap();
        c.resolve_metadata(&pkg("v22.10.0", "node-v22.10.0-linux-x64.tar.xz"))
            .await
            .unwrap();
        index.assert_async().await;
    }

    /// A release the index no longer lists is confirmed with a `HEAD` and
    /// carries no date — the case `deny_missing_timestamp` decides.
    #[tokio::test]
    async fn an_unlisted_release_that_exists_resolves_without_a_date() {
        let mut server = Server::new_async().await;
        let _index = server
            .mock("GET", "/index.tab")
            .with_body(INDEX_TAB)
            .create_async()
            .await;
        let head = server
            .mock("HEAD", "/v0.10.48/node-v0.10.48-linux-x64.tar.gz")
            .with_status(200)
            .create_async()
            .await;

        let meta = client(&server)
            .resolve_metadata(&pkg("v0.10.48", "node-v0.10.48-linux-x64.tar.gz"))
            .await
            .unwrap();
        head.assert_async().await;
        assert!(meta.published_at.is_none());
    }

    #[tokio::test]
    async fn an_unlisted_release_that_does_not_exist_is_not_found() {
        let mut server = Server::new_async().await;
        let _index = server
            .mock("GET", "/index.tab")
            .with_body(INDEX_TAB)
            .create_async()
            .await;
        let _head = server
            .mock("HEAD", "/v99.0.0/node-v99.0.0-linux-x64.tar.xz")
            .with_status(404)
            .create_async()
            .await;

        let result = client(&server)
            .resolve_metadata(&pkg("v99.0.0", "node-v99.0.0-linux-x64.tar.xz"))
            .await;
        assert!(matches!(result, Err(CoreError::NotFound(_))), "{result:?}");
    }

    /// An unreadable index is not a missing release: the file is checked
    /// directly and the release resolves undated.
    #[tokio::test]
    async fn an_index_outage_degrades_to_no_date_rather_than_not_found() {
        let mut server = Server::new_async().await;
        let _index = server
            .mock("GET", "/index.tab")
            .with_status(503)
            .create_async()
            .await;
        let _head = server
            .mock("HEAD", "/v22.11.0/SHASUMS256.txt")
            .with_status(200)
            .create_async()
            .await;

        let meta = client(&server)
            .resolve_metadata(&pkg("v22.11.0", "SHASUMS256.txt"))
            .await
            .unwrap();
        assert!(meta.published_at.is_none());
    }

    #[tokio::test]
    async fn fetch_artifact_streams_the_file_under_its_version_directory() {
        let mut server = Server::new_async().await;
        let _file = server
            .mock("GET", "/v22.11.0/SHASUMS256.txt")
            .with_header("cache-control", "max-age=86400")
            .with_body("abc  node-v22.11.0-linux-x64.tar.xz\n")
            .create_async()
            .await;

        let fetched = client(&server)
            .fetch_artifact(&pkg("v22.11.0", "SHASUMS256.txt"))
            .await
            .unwrap();
        assert_eq!(fetched.cache_control.as_deref(), Some("max-age=86400"));
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        let body: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
        assert_eq!(body, b"abc  node-v22.11.0-linux-x64.tar.xz\n");
    }

    #[tokio::test]
    async fn fetch_artifact_without_a_file_name_is_not_found() {
        let server = Server::new_async().await;
        let result = client(&server)
            .fetch_artifact(&PackageId::new("node", "node", "v22.11.0"))
            .await;
        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_maps_an_upstream_404_to_not_found() {
        let mut server = Server::new_async().await;
        let _file = server
            .mock("GET", "/v22.11.0/node-v22.11.0-plan9-x64.tar.xz")
            .with_status(404)
            .create_async()
            .await;
        let result = client(&server)
            .fetch_artifact(&pkg("v22.11.0", "node-v22.11.0-plan9-x64.tar.xz"))
            .await;
        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn index_tab_is_served_as_text_and_index_json_as_json() {
        let mut server = Server::new_async().await;
        let _tab = server
            .mock("GET", "/index.tab")
            .with_body(INDEX_TAB)
            .create_async()
            .await;
        let _json = server
            .mock("GET", "/index.json")
            .with_body(r#"[{"version":"v22.11.0","date":"2024-10-29","lts":"Jod"}]"#)
            .create_async()
            .await;
        let c = client(&server);

        let tab = c
            .fetch_version_document("node", DocumentKind::Versions)
            .await
            .unwrap();
        assert_eq!(tab.content_type, "text/plain; charset=utf-8");
        assert_eq!(tab.body.as_text(), Some(INDEX_TAB));

        let json = c
            .fetch_version_document("node", DocumentKind::INDEX_JSON)
            .await
            .unwrap();
        assert_eq!(json.content_type, "application/json");
        assert_eq!(json.body.as_json().unwrap()[0]["version"], "v22.11.0");

        let other = c.fetch_version_document("node", DocumentKind::LATEST).await;
        assert!(matches!(other, Err(CoreError::NotSupported(_))));
    }

    #[tokio::test]
    async fn list_versions_is_the_first_column_oldest_first() {
        let mut server = Server::new_async().await;
        let _tab = server
            .mock("GET", "/index.tab")
            .with_body(INDEX_TAB)
            .create_async()
            .await;
        let versions = client(&server).list_versions("node").await.unwrap();
        assert_eq!(versions, ["v22.10.0", "v22.11.0"]);
    }

    #[tokio::test]
    async fn the_nine_column_iojs_table_parses_too() {
        let mut server = Server::new_async().await;
        let _tab = server
            .mock("GET", "/index.tab")
            .with_body(IOJS_INDEX_TAB)
            .create_async()
            .await;
        let c = client(&server);
        assert_eq!(c.list_versions("iojs").await.unwrap(), ["v3.3.1"]);
        let meta = c
            .resolve_metadata(
                &PackageId::new("iojs", "iojs", "v3.3.1").with_artifact("SHASUMS256.txt"),
            )
            .await
            .unwrap();
        assert_eq!(
            meta.published_at.map(|d| d.to_rfc3339()),
            Some("2015-09-15T00:00:00+00:00".to_owned())
        );
        assert_eq!(meta.extra["lts"], serde_json::Value::Null);
    }
}
