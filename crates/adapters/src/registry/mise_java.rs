//! mise's JVM catalogue as a typed registry (RFC 0048 §6.3, §6.4).
//!
//! Two kinds of request, two kinds of host:
//!
//! - the **metadata documents**, `{upstream}/jvm/{type}/{os}/{arch}.json`
//!   (`https://mise-java.jdx.dev` by default), relayed with every row outside
//!   `vendor_hosts` dropped — the URLs left unchanged, because `mise.lock`
//!   records them (RFC 0048 §15 R1);
//! - the **archives**, fetched from the URL their row names, on one of eleven
//!   vendor hosts. A client asks for `dl/{host}/{path}`, and that is a *lookup
//!   key*, never a fetch target: the index built from the documents maps it to
//!   the row, and only the row's URL is ever fetched (§5.2, T3).
//!
//! Coordinates: one package, `java`; the version is the key mise builds for
//! the row ([`coordinate`]), the artifact the `(host, path)` key:
//!
//! ```text
//! PackageId { name: "java", version: "temurin-21.0.5+11.0.LTS",
//!             artifact: Some("github.com/adoptium/temurin21-binaries/…/x.tar.gz") }
//! → GET https://github.com/adoptium/temurin21-binaries/…/x.tar.gz (the row's URL)
//! ```
//!
//! **The index** is per process, built from every document this client has
//! fetched, and searched — documents fetched in the order the file name hints
//! at — on a miss: a lock-driven install asks for the archive without ever
//! reading a document, so the first download after a start is the common
//! case, not an edge one. What survives a restart or reaches a second replica
//! is the resolved `(host, path) → coordinate` pair, which
//! `ProxyService::canonical_coordinate` keeps in the cache store, and the
//! archives already held, which the download route serves by key.
//!
//! **Redirects** are followed here, never by reqwest: every hop must be
//! `https://`, land on `vendor_hosts` or `redirect_hosts`, and pass the SSRF
//! guard (§15 Q2). A refused hop is a `502` and nothing is cached.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::mise_java::{
        coordinate, count_dropped, created_at, filter_document, host_path, index_key,
        normalise_checksum, parse_document, search_order, vendor_allows, ChecksumClass, Platform,
        Row, JAVA_PACKAGE, MAX_DOCUMENT_BYTES,
    },
};

use super::http_client::{
    cache_control, fetch_document_bytes, new_http_client, no_redirect_client_pair,
    to_registry_error, UpstreamHttpOptions,
};

/// How long a fetched document answers a download lookup before a miss
/// fetches it again — the "cached 1 h" of RFC 0048 §5.2. The *served*
/// document has its own cache in `ProxyService`, under the registry's
/// `metadata_ttl`, and always asks upstream when that expires.
const DOCUMENT_TTL: Duration = Duration::from_secs(3600);

/// A refetched document this much smaller than the last good one is refused
/// and the last good one kept (RFC 0048 §6.3): a catalogue that answers `[]`
/// or a truncated array would otherwise drop every JDK at once.
const IMPLAUSIBLE_SHRINK: f64 = 0.75;

const MAX_REDIRECTS: usize = 5;

/// The version a download request carries before it is resolved: the client's
/// URL names no version, so the handler asks for this and
/// [`RegistryClient::canonical_coordinate`] answers the real one.
pub const UNRESOLVED_VERSION: &str = "_";

/// What mise-java needs beyond the upstream URL.
#[derive(Debug, Clone, Default)]
pub struct MiseJavaOptions {
    pub vendor_hosts: Vec<String>,
    pub redirect_hosts: Vec<String>,
    pub release_types: Vec<String>,
}

impl MiseJavaOptions {
    /// The registry's lists, each defaulted to RFC 0048 §4.1's when absent.
    pub fn from_config(
        vendor_hosts: Option<&[String]>,
        redirect_hosts: Option<&[String]>,
        release_types: Option<&[String]>,
    ) -> Self {
        use batlehub_core::services::mise_java::{DEFAULT_REDIRECT_HOSTS, DEFAULT_VENDOR_HOSTS};
        let or = |v: Option<&[String]>, d: &[&str]| {
            v.map(<[String]>::to_vec)
                .unwrap_or_else(|| d.iter().map(|s| (*s).to_owned()).collect())
        };
        Self {
            vendor_hosts: or(vendor_hosts, DEFAULT_VENDOR_HOSTS),
            redirect_hosts: or(redirect_hosts, DEFAULT_REDIRECT_HOSTS),
            release_types: or(release_types, &["ga"]),
        }
    }
}

/// One row of the index: the row, and the platforms whose documents list it.
#[derive(Debug, Clone)]
struct Indexed {
    row: Arc<Row>,
    platforms: BTreeSet<String>,
}

struct Doc {
    fetched_at: Instant,
    keys: Vec<String>,
}

#[derive(Default)]
struct State {
    docs: HashMap<Platform, Doc>,
    index: HashMap<String, Indexed>,
}

pub struct MiseJavaRegistryClient {
    http: super::http_client::UpstreamClient,
    /// Downloads: no redirects (followed by hand), no credential, public DNS.
    dl: reqwest::Client,
    base_url: String,
    registry: String,
    basic_auth: Option<(String, String)>,
    opts: MiseJavaOptions,
    state: RwLock<State>,
    /// One search at a time: a burst of cold downloads fetches each document
    /// once, not once per request.
    search: tokio::sync::Mutex<()>,
    /// Tests only: hosts answered by a local mock instead of over https.
    test_hosts: HashMap<String, String>,
}

impl MiseJavaRegistryClient {
    pub fn new(
        base_url: impl Into<String>,
        registry: impl Into<String>,
        opts: MiseJavaOptions,
        http_opts: &UpstreamHttpOptions,
    ) -> Result<Self, CoreError> {
        let (_, dl) = no_redirect_client_pair(http_opts)?;
        Ok(Self {
            http: new_http_client(Some(5), http_opts)?,
            dl,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            registry: registry.into(),
            basic_auth: http_opts.basic_auth.clone(),
            opts,
            state: RwLock::new(State::default()),
            search: tokio::sync::Mutex::new(()),
            test_hosts: HashMap::new(),
        })
    }

    /// Answer `host` from `base` (a local mock, `http://127.0.0.1:…`) as if it
    /// were `https://{host}`, without the SSRF guard — for that host only;
    /// every other hop is checked as in production. Not reachable from
    /// configuration.
    #[cfg(any(test, feature = "test-transport"))]
    pub fn with_test_host(mut self, host: &str, base: &str) -> Self {
        self.test_hosts.insert(
            host.to_ascii_lowercase(),
            base.trim_end_matches('/').to_owned(),
        );
        self.dl = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("test client");
        self
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Fetch one document, filter it by `vendor_hosts`, and index it. A
    /// document that does not parse, or shrank implausibly, keeps the last
    /// good one (§6.3).
    async fn load(&self, p: &Platform) -> Result<Vec<Row>, CoreError> {
        if !self.opts.release_types.contains(&p.release_type) {
            return Err(CoreError::NotFound(format!(
                "release type '{}' is not served by this registry",
                p.release_type
            )));
        }
        let url = format!("{}/{}", self.base_url, p.path());
        let fetched = fetch_document_bytes(
            self.http.basic_auth_get(&self.basic_auth, &url),
            "mise-java metadata document",
        )
        .await;
        let previous = self.document_rows(p);
        let rows = match fetched.map(|(body, _)| parse_document(&body)) {
            Ok(Some(rows)) => rows,
            Ok(None) => {
                tracing::warn!(registry = %self.registry, document = %p.path(),
                    "mise-java document does not parse (or exceeds {MAX_DOCUMENT_BYTES} bytes)");
                return previous.ok_or_else(|| {
                    CoreError::Registry(format!("{url} is not a mise-java document"))
                });
            }
            Err(e) => return Err(e),
        };
        let off_list: BTreeSet<String> = rows
            .iter()
            .filter_map(|r| host_path(&r.url))
            .filter(|hp| !vendor_allows(&index_key(hp), &self.opts.vendor_hosts))
            .filter_map(|hp| hp.split('/').next().map(str::to_owned))
            .collect();
        let (kept, dropped) = filter_document(rows, &self.opts.vendor_hosts);
        if let Some(prev) = previous {
            if (kept.len() as f64) < prev.len() as f64 * IMPLAUSIBLE_SHRINK {
                tracing::warn!(registry = %self.registry, document = %p.path(),
                    before = prev.len(), after = kept.len(),
                    "mise-java document shrank by more than a quarter; keeping the last good one");
                count_dropped(&self.registry, "implausible", prev.len() - kept.len());
                return Ok(prev);
            }
        }
        if dropped.host > 0 {
            tracing::warn!(registry = %self.registry, document = %p.path(),
                rows = dropped.host, hosts = ?off_list,
                "mise-java rows on hosts outside vendor_hosts dropped");
        }
        count_dropped(&self.registry, "host", dropped.host);
        count_dropped(&self.registry, "invalid", dropped.invalid);
        self.index(p, &kept);
        Ok(kept)
    }

    /// The rows of a document already indexed, if any.
    fn document_rows(&self, p: &Platform) -> Option<Vec<Row>> {
        let state = self.read();
        let doc = state.docs.get(p)?;
        Some(
            doc.keys
                .iter()
                .filter_map(|k| state.index.get(k))
                .map(|i| (*i.row).clone())
                .collect(),
        )
    }

    fn index(&self, p: &Platform, rows: &[Row]) {
        let tag = format!("{}/{}/{}", p.release_type, p.os, p.arch);
        let mut state = self.write();
        // The rows this platform listed last time leave first, so a withdrawn
        // row stops resolving at the next refresh.
        if let Some(old) = state.docs.remove(p) {
            for k in old.keys {
                if let Some(i) = state.index.get_mut(&k) {
                    i.platforms.remove(&tag);
                    if i.platforms.is_empty() {
                        state.index.remove(&k);
                    }
                }
            }
        }
        let mut keys = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(hp) = host_path(&row.url) else {
                continue;
            };
            let key = index_key(hp);
            let entry = state.index.entry(key.clone()).or_insert_with(|| Indexed {
                row: Arc::new(row.clone()),
                platforms: BTreeSet::new(),
            });
            entry.row = Arc::new(row.clone());
            entry.platforms.insert(tag.clone());
            keys.push(key);
        }
        state.docs.insert(
            p.clone(),
            Doc {
                fetched_at: Instant::now(),
                keys,
            },
        );
    }

    /// The row a download key names, searching the documents on a miss.
    async fn row_for(&self, key: &str) -> Result<Indexed, CoreError> {
        if let Some(i) = self.read().index.get(key) {
            return Ok(i.clone());
        }
        if !vendor_allows(key, &self.opts.vendor_hosts) {
            return Err(not_listed(key));
        }
        let _one = self.search.lock().await;
        for p in search_order(key, &self.opts.release_types) {
            if let Some(i) = self.read().index.get(key) {
                return Ok(i.clone());
            }
            let fresh = self
                .read()
                .docs
                .get(&p)
                .is_some_and(|d| d.fetched_at.elapsed() < DOCUMENT_TTL);
            if fresh {
                continue;
            }
            match self.load(&p).await {
                Ok(_) | Err(CoreError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        self.read()
            .index
            .get(key)
            .cloned()
            .ok_or_else(|| not_listed(key))
    }

    /// The download key a coordinate's artifact names, refused when absent.
    fn key_of(pkg: &PackageId) -> Result<&str, CoreError> {
        pkg.artifact.as_deref().ok_or_else(|| {
            CoreError::NotFound(format!(
                "a JVM build is one archive per row; name its URL under {}",
                pkg.version
            ))
        })
    }

    /// The URL to fetch for `url` and whether the hop is checked: a test host
    /// is answered by its mock, unchecked; everything else as it is.
    fn transport(&self, url: &reqwest::Url) -> (reqwest::Url, bool) {
        if let Some(base) = url
            .host_str()
            .and_then(|h| self.test_hosts.get(&h.to_ascii_lowercase()))
        {
            if let Ok(mapped) = reqwest::Url::parse(&format!("{base}{}", url.path())) {
                return (mapped, false);
            }
        }
        (url.clone(), true)
    }

    /// Whether a hop may be fetched: `https`, no port or userinfo, on a vendor
    /// prefix or a redirect host (RFC 0048 §15 Q2).
    fn hop_allowed(&self, url: &reqwest::Url) -> Result<(), CoreError> {
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let refuse = |why: &str| {
            tracing::warn!(registry = %self.registry, host = %host, "mise-java download refused: {why}");
            CoreError::Registry(format!(
                "mise-java: refusing a download hop to '{host}': {why}"
            ))
        };
        if url.scheme() != "https" {
            return Err(refuse("not https"));
        }
        if url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err(refuse("a port or credentials in the URL"));
        }
        let key = index_key(&format!("{host}{}", url.path()));
        let on_redirect_hosts = self
            .opts
            .redirect_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&host));
        if !on_redirect_hosts && !vendor_allows(&key, &self.opts.vendor_hosts) {
            return Err(refuse("neither in vendor_hosts nor in redirect_hosts"));
        }
        Ok(())
    }

    async fn fetch_row(&self, row: &Row) -> Result<reqwest::Response, CoreError> {
        let mut url = reqwest::Url::parse(&row.url)
            .map_err(|e| CoreError::Registry(format!("mise-java row URL '{}': {e}", row.url)))?;
        for _ in 0..=MAX_REDIRECTS {
            self.hop_allowed(&url)?;
            let (target, checked) = self.transport(&url);
            if checked {
                super::ssrf::ensure_public_url(&target).await?;
            }
            let resp = self
                .dl
                .get(target)
                .header(reqwest::header::ACCEPT, "*/*")
                .send()
                .await
                .map_err(to_registry_error)?;
            if !resp.status().is_redirection() {
                return Ok(resp);
            }
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    CoreError::Registry("a vendor redirect with no Location".to_owned())
                })?;
            url = url
                .join(location)
                .map_err(|e| CoreError::Registry(format!("redirect Location '{location}': {e}")))?;
        }
        Err(CoreError::Registry(format!(
            "too many redirects (>{MAX_REDIRECTS}) fetching {}",
            row.url
        )))
    }
}

fn not_listed(key: &str) -> CoreError {
    CoreError::NotFound(format!(
        "https://{key} is not a download any mise-java document of this registry lists"
    ))
}

#[async_trait]
impl RegistryClient for MiseJavaRegistryClient {
    fn registry_type(&self) -> &str {
        "mise-java"
    }

    /// The download key → the row's coordinate. The handler asks with
    /// [`UNRESOLVED_VERSION`]; any other version is already canonical.
    async fn canonical_coordinate(&self, pkg: &PackageId) -> Result<Option<PackageId>, CoreError> {
        if pkg.version != UNRESOLVED_VERSION || pkg.name != JAVA_PACKAGE {
            return Ok(None);
        }
        let key = Self::key_of(pkg)?;
        let found = self.row_for(key).await?;
        Ok(Some(
            PackageId::new(&pkg.registry, JAVA_PACKAGE, coordinate(&found.row)).with_artifact(key),
        ))
    }

    /// The row: its `created_at` for the age gate, its checksum (bare hex,
    /// SHA-256/512 or SHA-1) for the cache write, and the row itself under
    /// `extra["mise-java"]` for an air-gapped instance to list it from.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let key = Self::key_of(pkg)?;
        let found = self.row_for(key).await?;
        if coordinate(&found.row) != pkg.version {
            return Err(CoreError::NotFound(format!(
                "https://{key} is not a download of {}",
                pkg.version
            )));
        }
        let (checksum, class) = normalise_checksum(found.row.checksum.as_deref());
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: created_at(&found.row),
            download_url: Some(found.row.url.clone()),
            checksum,
            is_signed: None,
            extra: serde_json::json!({ "mise-java": {
                "row": *found.row,
                "platforms": found.platforms,
                "checksum": class.label(),
            }}),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let key = Self::key_of(pkg)?;
        let found = self.row_for(key).await?;
        let response = self.fetch_row(&found.row).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{} not found at its vendor",
                found.row.url
            )));
        }
        let response = response.error_for_status().map_err(to_registry_error)?;
        let class = normalise_checksum(found.row.checksum.as_deref()).1;
        if class != ChecksumClass::Strong {
            // Counted at the fill: a cache write follows unless the transfer
            // or the verification fails (RFC 0048 §15 Q10, T5).
            metrics::counter!("batlehub_mise_java_weak_downloads_total",
                "registry" => self.registry.clone(), "checksum" => class.label())
            .increment(1);
            tracing::info!(registry = %self.registry, coordinate = %pkg.version,
                checksum = class.label(),
                "mise-java archive cached with a weak checksum or none");
        }
        let cache_control = cache_control(&response);
        Ok(FetchedArtifact {
            stream: Box::pin(response.bytes_stream().map_err(to_registry_error)),
            cache_control,
        })
    }

    /// A metadata document, `java/{type}/{os}/{arch}` (`java` alone is
    /// `ga/linux/x86_64`, for the explorer), filtered by `vendor_hosts` and
    /// re-serialised through [`Row`].
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "mise-java has no '{kind}' document"
            )));
        }
        let platform = if package == JAVA_PACKAGE {
            Platform::parse(
                self.opts.release_types.first().map_or("ga", String::as_str),
                "linux",
                "x86_64",
            )
        } else {
            Platform::from_listing_name(package)
        }
        .ok_or_else(|| CoreError::NotFound(format!("'{package}' is not a mise-java document")))?;
        let rows = self.load(&platform).await?;
        let value = serde_json::to_value(rows)
            .map_err(|e| CoreError::Registry(format!("mise-java document: {e}")))?;
        Ok(VersionDocument::json(value))
    }

    /// The coordinates of the default document, oldest first by `created_at`.
    async fn list_versions(&self, _package: &str) -> Result<Vec<String>, CoreError> {
        let doc = self
            .fetch_version_document(JAVA_PACKAGE, DocumentKind::Versions)
            .await?;
        let mut rows: Vec<Row> = doc
            .body
            .as_json()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        rows.sort_by_key(created_at);
        let mut seen = BTreeSet::new();
        Ok(rows
            .iter()
            .map(coordinate)
            .filter(|c| seen.insert(c.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    const DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn row(url: &str, checksum: &str) -> serde_json::Value {
        serde_json::json!({"checksum": checksum, "created_at": "2025-03-28T22:04:32.217430",
            "features": [], "file_type": "tar.gz", "image_type": "jdk",
            "java_version": "21.0.5+11", "jvm_impl": "hotspot", "url": url,
            "vendor": "temurin", "version": "21.0.5+11.0.LTS", "unknown": 1})
    }

    const ADOPTIUM: &str = "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jdk_x64_linux_hotspot_21.0.5_11.tar.gz";

    async fn setup(rows: Vec<serde_json::Value>) -> (mockito::ServerGuard, MiseJavaRegistryClient) {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/jvm/ga/linux/x86_64.json")
            .with_body(serde_json::Value::Array(rows).to_string())
            // Ahead of the catch-all below on every request, not only the first.
            .expect_at_most(usize::MAX)
            .create_async()
            .await;
        server
            .mock("GET", mockito::Matcher::Regex("^/jvm/".into()))
            .with_status(404)
            .create_async()
            .await;
        let c = MiseJavaRegistryClient::new(
            server.url(),
            "jdk",
            MiseJavaOptions::from_config(None, None, None),
            &Default::default(),
        )
        .unwrap()
        .with_test_host("github.com", &server.url())
        .with_test_host("release-assets.githubusercontent.com", &server.url());
        (server, c)
    }

    fn dl(key: &str) -> PackageId {
        PackageId::new("jdk", JAVA_PACKAGE, UNRESOLVED_VERSION).with_artifact(key)
    }

    fn adoptium_key() -> String {
        index_key(host_path(ADOPTIUM).unwrap())
    }

    #[tokio::test]
    async fn the_document_is_filtered_and_served_through_its_fixed_struct() {
        let (_s, c) = setup(vec![
            row(ADOPTIUM, &format!("sha256:{DIGEST}")),
            row("https://github.com/someone/jdk/x.tar.gz", ""),
        ])
        .await;
        let doc = c
            .fetch_version_document("java/ga/linux/x86_64", DocumentKind::Versions)
            .await
            .unwrap();
        let rows = doc.body.as_json().unwrap().as_array().unwrap().clone();
        assert_eq!(
            rows.len(),
            1,
            "a row on another GitHub owner is never served"
        );
        assert_eq!(rows[0]["url"], ADOPTIUM, "URLs are served unchanged");
        assert!(rows[0].get("unknown").is_none());
        let err = c
            .fetch_version_document("java/ea/linux/x86_64", DocumentKind::Versions)
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::NotFound(_)),
            "ea is opt-in: {err:?}"
        );
    }

    #[tokio::test]
    async fn a_cold_download_finds_its_row_and_its_checksum_is_bare_hex() {
        let (_s, c) = setup(vec![row(ADOPTIUM, &format!("sha256:{DIGEST}"))]).await;
        let id = c
            .canonical_coordinate(&dl(&adoptium_key()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id.version, "temurin-21.0.5+11.0.LTS");
        let meta = c.resolve_metadata(&id).await.unwrap();
        assert_eq!(meta.checksum.as_deref(), Some(DIGEST));
        assert!(meta.published_at.is_some());
        assert_eq!(meta.extra["mise-java"]["platforms"][0], "ga/linux/x86_64");
        let other = PackageId::new("jdk", JAVA_PACKAGE, "zulu-21").with_artifact(adoptium_key());
        assert!(matches!(
            c.resolve_metadata(&other).await.unwrap_err(),
            CoreError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn a_download_not_in_any_document_is_404_without_upstream() {
        let (mut s, c) = setup(vec![row(ADOPTIUM, "")]).await;
        let never = s
            .mock(
                "GET",
                mockito::Matcher::Regex("^/(adoptium|latest)/".into()),
            )
            .expect(0)
            .create_async()
            .await;
        for key in [
            "github.com/adoptium/temurin21-binaries/releases/download/x/other.tar.gz",
            "169.254.169.254/latest/meta-data",
        ] {
            let err = c.canonical_coordinate(&dl(key)).await.unwrap_err();
            assert!(matches!(err, CoreError::NotFound(_)), "{key}: {err:?}");
        }
        never.assert_async().await;
    }

    #[tokio::test]
    async fn the_archive_follows_an_allowed_redirect() {
        let (mut s, c) = setup(vec![row(ADOPTIUM, "")]).await;
        let path = reqwest::Url::parse(ADOPTIUM).unwrap().path().to_owned();
        s.mock("GET", path.as_str())
            .with_status(302)
            .with_header(
                "location",
                "https://release-assets.githubusercontent.com/asset/1",
            )
            .create_async()
            .await;
        s.mock("GET", "/asset/1")
            .with_body("abc")
            .create_async()
            .await;
        let id = c
            .canonical_coordinate(&dl(&adoptium_key()))
            .await
            .unwrap()
            .unwrap();
        let fetched = c.fetch_artifact(&id).await.unwrap();
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        assert_eq!(chunks.concat(), b"abc");
    }

    #[tokio::test]
    async fn a_redirect_to_http_or_loopback_is_refused() {
        let (mut s, c) = setup(vec![row(ADOPTIUM, "")]).await;
        let path = reqwest::Url::parse(ADOPTIUM).unwrap().path().to_owned();
        for (location, why) in [
            (
                "http://release-assets.githubusercontent.com/asset/1",
                "not https",
            ),
            ("https://127.0.0.1/asset/1", "neither"),
            ("https://evil.example/asset/1", "neither"),
            (
                "https://release-assets.githubusercontent.com:8443/asset/1",
                "port",
            ),
        ] {
            let m = s
                .mock("GET", path.as_str())
                .with_status(302)
                .with_header("location", location)
                .create_async()
                .await;
            let id = c
                .canonical_coordinate(&dl(&adoptium_key()))
                .await
                .unwrap()
                .unwrap();
            let err = c.fetch_artifact(&id).await.err().expect(location);
            assert!(
                matches!(&err, CoreError::Registry(m) if m.contains(why)),
                "{location}: {err:?}"
            );
            m.remove_async().await;
        }
    }

    #[tokio::test]
    async fn an_implausible_catalogue_keeps_the_last_good_one() {
        let mut server = Server::new_async().await;
        let rows: Vec<_> = (0..8)
            .map(|i| row(&format!("https://github.com/adoptium/t/{i}.tar.gz"), ""))
            .collect();
        let good = server
            .mock("GET", "/jvm/ga/linux/x86_64.json")
            .with_body(serde_json::Value::Array(rows).to_string())
            .create_async()
            .await;
        let c = MiseJavaRegistryClient::new(
            server.url(),
            "jdk",
            MiseJavaOptions::from_config(None, None, None),
            &Default::default(),
        )
        .unwrap();
        let name = "java/ga/linux/x86_64";
        let first = c
            .fetch_version_document(name, DocumentKind::Versions)
            .await
            .unwrap();
        assert_eq!(first.body.as_json().unwrap().as_array().unwrap().len(), 8);
        good.remove_async().await;
        for body in ["[]", "{\"truncated\""] {
            let m = server
                .mock("GET", "/jvm/ga/linux/x86_64.json")
                .with_body(body)
                .create_async()
                .await;
            let doc = c
                .fetch_version_document(name, DocumentKind::Versions)
                .await
                .unwrap();
            assert_eq!(
                doc.body.as_json().unwrap().as_array().unwrap().len(),
                8,
                "{body}"
            );
            m.remove_async().await;
        }
    }
}
