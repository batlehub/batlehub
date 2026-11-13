//! Go's toolchain distribution as a typed registry (RFC 0048 §6.7).
//!
//! Two hosts, one protocol. The archives and their `.sha256` siblings live
//! under the download base (`https://dl.google.com/go`, what mise's
//! `go.download_mirror` defaults to); the release document — every release,
//! every file, a `sha256` and a `size` each, no dates — is
//! `https://go.dev/dl/?mode=json&include=all` (`metadata_url`).
//!
//! Coordinates: one package, `go`; one version per release, without the `go`
//! prefix (`1.25.4`, as mise spells it); the file name as the artifact:
//!
//! ```text
//! PackageId { name: "go", version: "1.25.4", artifact: Some("go1.25.4.linux-amd64.tar.gz") }
//! → GET {base}/go1.25.4.linux-amd64.tar.gz
//! ```
//!
//! The document decides what exists. A file it does not list is `NotFound`
//! without an upstream request, so a client cannot make this fetch an
//! arbitrary path; a listed archive carries the document's `sha256` as its
//! checksum, which the cache write verifies; and a `.sha256` is answered from
//! the document rather than fetched, so the digest the client checks and the
//! digest the cache verified are the same value from a host other than the
//! archive's.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::godist::{bare_version, parse_file_name, parse_releases, Release},
};

use super::http_client::{
    basic_auth_get, cache_control, encode_path, fetch_document_bytes, new_http_client,
    to_registry_error, UpstreamHttpOptions,
};

/// How long one fetched release document answers `resolve_metadata`. The
/// *served* document has its own cache in `ProxyService`, keyed by the
/// registry's `metadata_ttl`; this one is the file → digest table.
const DOCUMENT_TTL: Duration = Duration::from_secs(300);

/// `filename` → `(sha256, size)`.
type Rows = HashMap<String, (String, u64)>;

struct CachedRows {
    fetched_at: Instant,
    rows: Rows,
}

pub struct GoDistRegistryClient {
    http: reqwest::Client,
    base_url: String,
    metadata_url: String,
    basic_auth: Option<(String, String)>,
    rows: Mutex<Option<CachedRows>>,
}

impl GoDistRegistryClient {
    pub fn new(
        base_url: impl Into<String>,
        metadata_url: impl Into<String>,
        opts: &UpstreamHttpOptions,
    ) -> Result<Self, CoreError> {
        Ok(Self {
            http: new_http_client(Some(5), opts)?,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            metadata_url: metadata_url.into(),
            basic_auth: opts.basic_auth.clone(),
            rows: Mutex::new(None),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    async fn releases(&self) -> Result<Vec<Release>, CoreError> {
        let (body, _) =
            fetch_document_bytes(self.get(&self.metadata_url), "Go release document").await?;
        // shortcut: the body is read whole and then capped; a streaming cap
        // would matter only for an upstream that is already hostile.
        parse_releases(&body).ok_or_else(|| {
            CoreError::Registry(format!(
                "the Go release document at {} is not a release array of at most {} bytes",
                self.metadata_url,
                batlehub_core::services::godist::MAX_DOCUMENT_BYTES
            ))
        })
    }

    async fn rows(&self) -> Result<Rows, CoreError> {
        if let Some(cached) = self.rows.lock().expect("rows lock").as_ref() {
            if cached.fetched_at.elapsed() < DOCUMENT_TTL {
                return Ok(cached.rows.clone());
            }
        }
        let rows: Rows = self
            .releases()
            .await?
            .into_iter()
            .flat_map(|r| r.files)
            .map(|f| (f.filename, (f.sha256.to_ascii_lowercase(), f.size)))
            .collect();
        *self.rows.lock().expect("rows lock") = Some(CachedRows {
            fetched_at: Instant::now(),
            rows: rows.clone(),
        });
        Ok(rows)
    }

    /// The archive a file name names, checked against the coordinate's version.
    fn archive_of(pkg: &PackageId) -> Result<(&str, bool), CoreError> {
        let file = pkg.artifact.as_deref().ok_or_else(|| {
            CoreError::NotFound(format!(
                "a Go release is a set of archives; name one under {}",
                pkg.version
            ))
        })?;
        match parse_file_name(file) {
            Some((version, archive, checksum)) if version == pkg.version => Ok((archive, checksum)),
            _ => Err(CoreError::NotFound(format!(
                "'{file}' is not a Go {} archive",
                pkg.version
            ))),
        }
    }
}

#[async_trait]
impl RegistryClient for GoDistRegistryClient {
    fn registry_type(&self) -> &str {
        "godist"
    }

    /// The file's row in the release document. No date: the document has none,
    /// which is why config refuses an age gate on this kind.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let (archive, is_checksum) = Self::archive_of(pkg)?;
        let rows = self.rows().await?;
        let Some((sha256, size)) = rows.get(archive) else {
            return Err(CoreError::NotFound(format!(
                "{archive} is not in the Go release document"
            )));
        };
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: None,
            download_url: None,
            // Bare hex, the form `integrity::parse_expected` reads. The
            // `.sha256` is the digest itself, so it carries none.
            checksum: (!is_checksum && !sha256.is_empty()).then(|| sha256.clone()),
            is_signed: None,
            extra: serde_json::json!({ "sha256": sha256, "size": size }),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let (archive, is_checksum) = Self::archive_of(pkg)?;
        if is_checksum {
            // dl.google.com serves the 64 hex digits and nothing else; mise
            // reads the body as the whole digest (`go.rs:126-128`, `:137-138`).
            let rows = self.rows().await?;
            let (sha256, _) = rows.get(archive).ok_or_else(|| {
                CoreError::NotFound(format!("{archive} is not in the Go release document"))
            })?;
            let body = bytes::Bytes::from(sha256.clone());
            return Ok(FetchedArtifact {
                stream: Box::pin(futures::stream::once(
                    async move { Ok::<_, CoreError>(body) },
                )),
                cache_control: None,
            });
        }
        let url = format!("{}/{}", self.base_url, encode_path(archive));
        let response = self.get(&url).send().await.map_err(to_registry_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{archive} not found upstream")));
        }
        let response = response.error_for_status().map_err(to_registry_error)?;
        let cache_control = cache_control(&response);
        Ok(FetchedArtifact {
            stream: Box::pin(response.bytes_stream().map_err(to_registry_error)),
            cache_control,
        })
    }

    /// The release document, re-serialised through its fixed struct so a field
    /// this proxy does not know never reaches a client.
    async fn fetch_version_document(
        &self,
        _package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        match kind {
            DocumentKind::Versions => {
                let value = serde_json::to_value(self.releases().await?)
                    .map_err(|e| CoreError::Registry(format!("Go release document: {e}")))?;
                Ok(VersionDocument::json(value))
            }
            other => Err(CoreError::NotSupported(format!(
                "Go's distribution has no '{other}' listing document"
            ))),
        }
    }

    /// Every release, oldest first as the trait asks — the document is newest
    /// first.
    async fn list_versions(&self, _package: &str) -> Result<Vec<String>, CoreError> {
        let mut versions: Vec<String> = self
            .releases()
            .await?
            .into_iter()
            .map(|r| bare_version(&r.version).to_owned())
            .collect();
        versions.reverse();
        Ok(versions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    const DIGEST: &str = "9fa5ffeda4170de60f67f3aa0f824e426421ba724c21e133c1e35d6159ca1bec";

    fn document() -> String {
        serde_json::json!([
            {"version": "go1.25.4", "stable": true, "unknown": "dropped", "files": [
                {"filename": "go1.25.4.linux-amd64.tar.gz", "os": "linux", "arch": "amd64",
                 "version": "go1.25.4", "sha256": DIGEST, "size": 3, "kind": "archive"}
            ]},
            {"version": "go1.25.3", "stable": true, "files": []}
        ])
        .to_string()
    }

    async fn setup() -> (mockito::ServerGuard, GoDistRegistryClient) {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/dl/")
            .match_query(mockito::Matcher::Any)
            .with_body(document())
            .create_async()
            .await;
        let c = GoDistRegistryClient::new(
            format!("{}/go", server.url()),
            format!("{}/dl/?mode=json&include=all", server.url()),
            &Default::default(),
        )
        .unwrap();
        (server, c)
    }

    fn pkg(file: &str) -> PackageId {
        PackageId::new("go", "go", "1.25.4").with_artifact(file)
    }

    #[tokio::test]
    async fn a_listed_archive_carries_the_document_digest_as_bare_hex() {
        let (_s, c) = setup().await;
        let meta = c
            .resolve_metadata(&pkg("go1.25.4.linux-amd64.tar.gz"))
            .await
            .unwrap();
        assert_eq!(meta.checksum.as_deref(), Some(DIGEST));
        assert!(meta.published_at.is_none());
    }

    #[tokio::test]
    async fn an_unlisted_go_file_is_404_without_upstream() {
        let (mut s, c) = setup().await;
        let never = s
            .mock("GET", "/go/go1.25.4.plan9-amd64.tar.gz")
            .expect(0)
            .create_async()
            .await;
        let err = c
            .resolve_metadata(&pkg("go1.25.4.plan9-amd64.tar.gz"))
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "{err:?}");
        never.assert_async().await;
    }

    #[tokio::test]
    async fn a_file_of_another_version_is_not_this_coordinate() {
        let (_s, c) = setup().await;
        let err = c
            .resolve_metadata(&pkg("go1.25.3.linux-amd64.tar.gz"))
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn the_sha256_is_answered_from_the_document() {
        let (mut s, c) = setup().await;
        let never = s
            .mock("GET", "/go/go1.25.4.linux-amd64.tar.gz.sha256")
            .expect(0)
            .create_async()
            .await;
        let fetched = c
            .fetch_artifact(&pkg("go1.25.4.linux-amd64.tar.gz.sha256"))
            .await
            .unwrap();
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        assert_eq!(chunks.concat(), DIGEST.as_bytes());
        never.assert_async().await;
        let meta = c
            .resolve_metadata(&pkg("go1.25.4.linux-amd64.tar.gz.sha256"))
            .await
            .unwrap();
        assert_eq!(meta.checksum, None);
    }

    #[tokio::test]
    async fn the_archive_streams_from_the_download_base() {
        let (mut s, c) = setup().await;
        s.mock("GET", "/go/go1.25.4.linux-amd64.tar.gz")
            .with_body("abc")
            .create_async()
            .await;
        let fetched = c
            .fetch_artifact(&pkg("go1.25.4.linux-amd64.tar.gz"))
            .await
            .unwrap();
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        assert_eq!(chunks.concat(), b"abc");
    }

    #[tokio::test]
    async fn the_document_is_served_through_its_fixed_struct() {
        let (_s, c) = setup().await;
        let doc = c
            .fetch_version_document("go", DocumentKind::Versions)
            .await
            .unwrap();
        let json = doc.body.as_json().unwrap();
        assert_eq!(json[0]["version"], "go1.25.4");
        assert!(json[0].get("unknown").is_none(), "{json}");
        assert_eq!(c.list_versions("go").await.unwrap(), ["1.25.3", "1.25.4"]);
    }
}
