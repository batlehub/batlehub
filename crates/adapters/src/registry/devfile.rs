//! A devfile registry — `registry.devfile.io` by default (RFC 0035 §6.4).
//!
//! Coordinates (§4.3): the package is a stack, the version a stack version, and
//! every file of a version is an artifact of it —
//!
//! ```text
//! PackageId { name: "go", version: "2.6.0", artifact: Some("manifest") }
//!     → GET {base}/v2/devfile-catalog/go/manifests/2.6.0
//! PackageId { name: "go", version: "2.6.0", artifact: Some("layer/archive.tar") }
//!     → GET {base}/v2/devfile-catalog/go/blobs/sha256:5144…
//! PackageId { name: "nodejs", version: "2.2.1", artifact: Some("starter-projects/nodejs-starter.zip") }
//!     → GET {base}/devfiles/nodejs/2.2.1/starter-projects/nodejs-starter
//! ```
//!
//! The namespace and tag come from the version's `links.self` in the v2 index,
//! never from a guess, and the digest of each layer from the manifest.
//!
//! **Every body is buffered and verified before it is handed on.** The client
//! this protocol has, `registry-library`, checks each blob against its digest
//! and then leaves a failed file on disk and exits `0` (§2.3), so the check
//! that has consequences is this one. A manifest must hash to upstream's
//! `Docker-Content-Digest`; a layer to its descriptor. The objects are small —
//! 414 B manifests, 617 B to 2.6 KB layers observed — and capped at
//! [`MAX_OBJECT_BYTES`], so buffering costs nothing a stream would save.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::devfile::{
        self, default_version, find_stack, index_document, is_index_address, layer_artifact,
        manifest_descriptors, self_link, starter_of, starter_projects, validate_stack,
        validate_starter, validate_version, version_entry, versions_of, DEVFILE_TITLE,
        LAYER_PREFIX, MANIFEST_ACCEPT, MANIFEST_ARTIFACT,
    },
};

use super::http_client::{
    basic_auth_get, cache_control, ensure_url_under_base, fetch_json_document, new_http_client,
    to_registry_error, UpstreamHttpOptions,
};

/// How long one fetched v2 index answers `resolve_metadata` before it is
/// re-read. The *served* index has its own cache in `ProxyService`, keyed by
/// the registry's `metadata_ttl`; this one maps coordinates to OCI references,
/// and a `pull` resolves a stack's manifest and every layer through it.
const INDEX_TTL: Duration = Duration::from_secs(300);

/// The largest manifest, layer or starter archive this client will buffer.
/// Three orders of magnitude above anything `registry.devfile.io` serves.
pub const MAX_OBJECT_BYTES: usize = 32 * 1024 * 1024;

struct CachedIndex {
    fetched_at: Instant,
    value: Arc<serde_json::Value>,
}

pub struct DevfileRegistryClient {
    http: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
    index: Mutex<Option<CachedIndex>>,
}

/// What `resolve_metadata` learned about one version, beyond the coordinate.
struct Resolved {
    namespace: String,
    tag: String,
    record: serde_json::Value,
    /// The stack's index entry: `displayName`, `icon`, `projectType`, … — the
    /// fields its versions share.
    stack: serde_json::Value,
}

impl DevfileRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        Ok(Self {
            http: new_http_client(Some(5), opts)?,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            basic_auth: opts.basic_auth.clone(),
            index: Mutex::new(None),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// The v2 index of stacks, from the in-process copy when it is fresh.
    async fn v2_index(&self) -> Result<Arc<serde_json::Value>, CoreError> {
        if let Some(cached) = self.index.lock().expect("index lock").as_ref() {
            if cached.fetched_at.elapsed() < INDEX_TTL {
                return Ok(Arc::clone(&cached.value));
            }
        }
        let url = format!("{}/v2index", self.base_url);
        let doc = fetch_json_document(self.get(&url), "devfile v2 index").await?;
        let value = Arc::new(doc.body.as_json().cloned().unwrap_or_default());
        *self.index.lock().expect("index lock") = Some(CachedIndex {
            fetched_at: Instant::now(),
            value: Arc::clone(&value),
        });
        Ok(value)
    }

    /// The stack's version record and its OCI reference. `latest` — the
    /// placeholder a listing route and a versionless request use — is the
    /// stack's default version.
    async fn resolve(&self, stack: &str, version: &str) -> Result<(String, Resolved), CoreError> {
        validate_stack(stack)?;
        let index = self.v2_index().await?;
        let entry = find_stack(&index, stack).ok_or_else(|| {
            CoreError::NotFound(format!("stack '{stack}' is not in the devfile registry"))
        })?;
        let version = if version == "latest" {
            default_version(entry)
                .ok_or_else(|| CoreError::NotFound(format!("stack '{stack}' has no versions")))?
                .to_owned()
        } else {
            validate_version(version)?;
            version.to_owned()
        };
        let record = version_entry(entry, &version).ok_or_else(|| {
            CoreError::NotFound(format!(
                "the requested version {version} for stack {stack} does not exist in the registry"
            ))
        })?;
        let link = record
            .pointer("/links/self")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CoreError::Registry(format!("devfile index names no link for {stack}@{version}"))
            })?;
        let (namespace, tag) = self_link(link, stack)?;
        Ok((
            version,
            Resolved {
                namespace,
                tag,
                record: record.clone(),
                stack: entry.clone(),
            },
        ))
    }

    fn manifest_url(&self, ns: &str, stack: &str, reference: &str) -> String {
        format!("{}/v2/{ns}/{stack}/manifests/{reference}", self.base_url)
    }

    fn blob_url(&self, ns: &str, stack: &str, digest: &str) -> String {
        format!("{}/v2/{ns}/{stack}/blobs/{digest}", self.base_url)
    }

    /// GET a manifest and hold it to the digest upstream announced for it.
    ///
    /// containerd compares the manifest with the `Docker-Content-Digest` of its
    /// `HEAD`, so a body that hashes to something else would fail on the
    /// client; failing here keeps it out of the cache as well.
    async fn fetch_manifest(&self, url: &str) -> Result<(bytes::Bytes, String), CoreError> {
        let resp = self
            .get(url)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT)
            .send()
            .await
            .map_err(to_registry_error)?;
        let announced = resp
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let (body, _) = read_bounded(resp, url).await?;
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
        if let Some(announced) = announced {
            if announced != digest {
                return Err(CoreError::Registry(format!(
                    "devfile manifest {url} hashes to {digest}, upstream announced {announced}"
                )));
            }
        }
        Ok((body, digest))
    }
}

/// Read a response body whole, up to [`MAX_OBJECT_BYTES`], mapping `404` to
/// `NotFound`. Returns the body and its `Cache-Control`.
async fn read_bounded(
    resp: reqwest::Response,
    what: &str,
) -> Result<(bytes::Bytes, Option<String>), CoreError> {
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(CoreError::NotFound(format!("{what} not found upstream")));
    }
    let resp = resp.error_for_status().map_err(to_registry_error)?;
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_OBJECT_BYTES as u64)
    {
        return Err(CoreError::Registry(format!(
            "{what} is larger than {MAX_OBJECT_BYTES} bytes"
        )));
    }
    let cc = cache_control(&resp);
    let mut body = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.map_err(to_registry_error)? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_OBJECT_BYTES {
            return Err(CoreError::Registry(format!(
                "{what} is larger than {MAX_OBJECT_BYTES} bytes"
            )));
        }
    }
    Ok((bytes::Bytes::from(body), cc))
}

#[async_trait]
impl RegistryClient for DevfileRegistryClient {
    fn registry_type(&self) -> &str {
        "devfile"
    }

    /// The version's record and OCI reference out of the v2 index, and — for
    /// the manifest and a layer — the digest out of the manifest, as the bare
    /// hex `integrity::parse_expected` reads.
    ///
    /// `published_at` is `None`: upstream's `lastModified` is the time it last
    /// rebuilt the whole registry, the same instant on every version, so it
    /// would date every stack by the rebuild (RFC 0035 §6.7). The age gate
    /// treats a devfile version as undated and follows the registry's
    /// `deny_missing_timestamp`.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let (version, r) = self.resolve(&pkg.name, &pkg.version).await?;
        let stack = pkg.name.as_str();
        // The facts shape a bundle import files from the bytes
        // (`devfile::import_facts`), plus what only the index knows — so the
        // web layer's digest lookup and an air-gapped composition read one
        // place whichever side wrote it (RFC 0035 §6.8).
        let field = |k: &str| {
            r.record
                .get(k)
                .or_else(|| r.stack.get(k))
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        };
        let mut metadata = serde_json::Map::new();
        for k in [
            "displayName",
            "description",
            "icon",
            "tags",
            "projectType",
            "language",
            "provider",
            "architectures",
        ] {
            let v = field(k);
            if !v.is_null() {
                metadata.insert(k.to_owned(), v);
            }
        }
        let mut extra = serde_json::json!({ "devfile": {
            "namespace": r.namespace,
            "tag": r.tag,
            "default": r.record.get("default").and_then(|v| v.as_bool()).unwrap_or(false),
            "starterProjects": starter_projects(&r.record),
            "schemaVersion": r.record.get("schemaVersion"),
            "metadata": metadata,
        } });

        let (download_url, checksum) = match pkg.artifact.as_deref() {
            Some(artifact) if artifact.starts_with(devfile::STARTER_PREFIX) => {
                let name = starter_of(artifact).ok_or_else(|| {
                    CoreError::InvalidInput(format!("'{artifact}' is not a starter project"))
                })?;
                validate_starter(name)?;
                if !starter_projects(&r.record).contains(&name) {
                    return Err(CoreError::NotFound(format!(
                        "the starter project '{name}' does not exist under the stack '{stack}'"
                    )));
                }
                (
                    Some(format!(
                        "{}/devfiles/{stack}/{version}/starter-projects/{name}",
                        self.base_url
                    )),
                    // Generated from a git tree on request; nothing announces
                    // a digest for it.
                    None,
                )
            }
            artifact => {
                let manifest_url = self.manifest_url(&r.namespace, stack, &r.tag);
                let (body, digest) = self.fetch_manifest(&manifest_url).await?;
                let descriptors = manifest_descriptors(&body)?;
                extra["devfile"]["manifestDigest"] = serde_json::Value::String(digest.clone());
                extra["devfile"]["layers"] = devfile::layers_json(&descriptors);
                match artifact {
                    None => (None, None),
                    Some(MANIFEST_ARTIFACT) => (
                        Some(manifest_url),
                        Some(digest.trim_start_matches("sha256:").to_owned()),
                    ),
                    Some(a) => {
                        let title = a.strip_prefix(LAYER_PREFIX).ok_or_else(|| {
                            CoreError::NotFound(format!("'{a}' is not an artifact of {stack}"))
                        })?;
                        let layer = descriptors
                            .iter()
                            .find(|d| d.title.as_deref() == Some(title))
                            .ok_or_else(|| {
                                CoreError::NotFound(format!(
                                    "{stack}@{version} has no layer '{title}'"
                                ))
                            })?;
                        (
                            Some(self.blob_url(&r.namespace, stack, &layer.digest)),
                            Some(layer.hex().to_owned()),
                        )
                    }
                }
            }
        };

        Ok(PackageMetadata {
            id: PackageId {
                version,
                ..pkg.clone()
            },
            published_at: None,
            download_url,
            checksum,
            is_signed: None,
            extra,
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let resolved = self.resolve_metadata(pkg).await?;
        match self.fetch_artifact_resolved(pkg, &resolved).await? {
            Some(fetched) => Ok(fetched),
            None => Err(CoreError::NotFound(format!(
                "{pkg} names no file of the stack; ask for the manifest, a layer or a starter project"
            ))),
        }
    }

    /// Buffered, verified, then handed on as one chunk. The URL is the one
    /// `resolve_metadata` built from this registry's own base, and is checked
    /// against it again, because a cached `PackageMetadata` is a document too.
    async fn fetch_artifact_resolved(
        &self,
        pkg: &PackageId,
        resolved: &PackageMetadata,
    ) -> Result<Option<FetchedArtifact>, CoreError> {
        let Some(url) = resolved.download_url.as_deref() else {
            return Ok(None);
        };
        ensure_url_under_base(url, &self.base_url)?;
        let mut req = self.get(url);
        if pkg.artifact.as_deref() == Some(MANIFEST_ARTIFACT) {
            req = req.header(reqwest::header::ACCEPT, MANIFEST_ACCEPT);
        }
        let resp = req.send().await.map_err(to_registry_error)?;
        let (body, cache_control) = read_bounded(resp, url).await?;
        if let Some(expected) = resolved.checksum.as_deref() {
            let actual = hex::encode(Sha256::digest(&body));
            if actual != expected {
                return Err(CoreError::Registry(format!(
                    "{pkg} from {url} hashes to sha256:{actual}, expected sha256:{expected}; not served"
                )));
            }
        }
        let stream = futures::stream::once(async move { Ok::<_, CoreError>(body) });
        Ok(Some(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        }))
    }

    /// An index document by its address (`v2index/all?arch=amd64`), or — for
    /// the console's discovery read, which names a stack — that stack's entry
    /// of the v2 index.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if is_index_address(package) {
            let path = package.split('?').next().unwrap_or(package);
            if index_document(path) != Some(kind) {
                return Err(CoreError::NotSupported(format!(
                    "'{package}' is not a '{kind}' document"
                )));
            }
            let url = format!("{}/{package}", self.base_url);
            return fetch_json_document(self.get(&url), &format!("devfile index '{package}'"))
                .await;
        }
        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "a devfile registry has no '{kind}' document for a stack"
            )));
        }
        validate_stack(package)?;
        let index = self.v2_index().await?;
        let entry = find_stack(&index, package).ok_or_else(|| {
            CoreError::NotFound(format!("stack '{package}' is not in the devfile registry"))
        })?;
        Ok(VersionDocument::json(entry.clone()))
    }

    /// The stack's versions, oldest first as the trait asks — the index lists
    /// them newest first.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        validate_stack(package)?;
        let index = self.v2_index().await?;
        let mut versions: Vec<String> = find_stack(&index, package)
            .map(|e| versions_of(e).into_iter().map(str::to_owned).collect())
            .unwrap_or_default();
        versions.reverse();
        Ok(versions)
    }
}

/// The artifact a layer title is cached under — re-exported for the handlers,
/// which name the devfile layer on the REST route.
pub fn devfile_layer() -> String {
    layer_artifact(DEVFILE_TITLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use mockito::{Matcher, Server};

    const DEVFILE: &[u8] = b"schemaVersion: 2.2.2\nmetadata:\n  name: nodejs\n  version: 2.2.1\n";
    const ARCHIVE: &[u8] = b"not really a tar";

    fn sha(bytes: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }

    fn manifest() -> String {
        serde_json::json!({
            "schemaVersion": 2,
            "config": {"mediaType": "application/vnd.devfileio.devfile.config.v2+json",
                       "digest": sha(b"{}"), "size": 2},
            "layers": [
                {"mediaType": "application/x-tar", "digest": sha(ARCHIVE), "size": ARCHIVE.len(),
                 "annotations": {"org.opencontainers.image.title": "archive.tar"}},
                {"mediaType": "application/vnd.devfileio.devfile.layer.v1", "digest": sha(DEVFILE),
                 "size": DEVFILE.len(), "annotations": {"org.opencontainers.image.title": "devfile.yaml"}}
            ]
        })
        .to_string()
    }

    fn v2index() -> String {
        serde_json::json!([
            {"name": "nodejs", "type": "stack", "versions": [
                {"version": "2.2.1", "default": true, "links": {"self": "devfile-catalog/nodejs:2.2.1"},
                 "starterProjects": ["nodejs-starter"]},
                {"version": "2.2.0", "links": {"self": "devfile-catalog/nodejs:2.2.0"}}
            ]},
            {"name": "evil", "type": "stack", "versions": [
                {"version": "1.0.0", "default": true, "links": {"self": "../../x/evil:1.0.0"}}
            ]}
        ])
        .to_string()
    }

    async fn upstream() -> mockito::ServerGuard {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/v2index")
            .with_body(v2index())
            .create_async()
            .await;
        let m = manifest();
        server
            .mock("GET", "/v2/devfile-catalog/nodejs/manifests/2.2.1")
            .match_header("accept", Matcher::Regex("oci.image.manifest".into()))
            .with_header("docker-content-digest", &sha(m.as_bytes()))
            .with_body(m)
            .create_async()
            .await;
        server
            .mock(
                "GET",
                format!("/v2/devfile-catalog/nodejs/blobs/{}", sha(DEVFILE)).as_str(),
            )
            .with_body(DEVFILE)
            .create_async()
            .await;
        server
    }

    fn client(server: &mockito::ServerGuard) -> DevfileRegistryClient {
        DevfileRegistryClient::new(server.url(), &Default::default()).unwrap()
    }

    async fn body(fetched: FetchedArtifact) -> Vec<u8> {
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        chunks.concat()
    }

    #[tokio::test]
    async fn a_layer_resolves_to_its_blob_with_a_bare_hex_checksum() {
        let server = upstream().await;
        let pkg = PackageId::new("d", "nodejs", "2.2.1").with_artifact("layer/devfile.yaml");
        let meta = client(&server).resolve_metadata(&pkg).await.unwrap();
        assert_eq!(meta.checksum.as_deref(), Some(&sha(DEVFILE)[7..]));
        assert!(meta
            .download_url
            .unwrap()
            .ends_with(&format!("/blobs/{}", sha(DEVFILE))));
        assert!(meta.published_at.is_none());
        assert_eq!(meta.extra["devfile"]["namespace"], "devfile-catalog");
    }

    #[tokio::test]
    async fn latest_is_the_default_version() {
        let server = upstream().await;
        let pkg = PackageId::new("d", "nodejs", "latest").with_artifact(MANIFEST_ARTIFACT);
        let meta = client(&server).resolve_metadata(&pkg).await.unwrap();
        assert_eq!(meta.id.version, "2.2.1");
        assert_eq!(
            meta.checksum.as_deref(),
            Some(&sha(manifest().as_bytes())[7..])
        );
    }

    #[tokio::test]
    async fn the_layer_bytes_are_fetched_and_verified() {
        let server = upstream().await;
        let c = client(&server);
        let pkg = PackageId::new("d", "nodejs", "2.2.1").with_artifact("layer/devfile.yaml");
        assert_eq!(body(c.fetch_artifact(&pkg).await.unwrap()).await, DEVFILE);
    }

    #[tokio::test]
    async fn an_altered_layer_is_refused_before_a_byte_is_handed_on() {
        let server = upstream().await;
        let c = client(&server);
        let pkg = PackageId::new("d", "nodejs", "2.2.1").with_artifact("layer/devfile.yaml");
        let mut meta = c.resolve_metadata(&pkg).await.unwrap();
        meta.checksum = Some("0".repeat(64));
        let err = c.fetch_artifact_resolved(&pkg, &meta).await.err().unwrap();
        assert!(
            matches!(err, CoreError::Registry(ref m) if m.contains("not served")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_manifest_that_disagrees_with_its_announced_digest_is_refused() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/v2index")
            .with_body(v2index())
            .create_async()
            .await;
        server
            .mock("GET", "/v2/devfile-catalog/nodejs/manifests/2.2.1")
            .with_header("docker-content-digest", &sha(b"something else"))
            .with_body(manifest())
            .create_async()
            .await;
        let pkg = PackageId::new("d", "nodejs", "2.2.1").with_artifact(MANIFEST_ARTIFACT);
        let err = client(&server).resolve_metadata(&pkg).await.err().unwrap();
        assert!(
            matches!(err, CoreError::Registry(ref m) if m.contains("announced")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_missing_version_or_layer_or_starter_is_not_found() {
        let server = upstream().await;
        let c = client(&server);
        for pkg in [
            PackageId::new("d", "nodejs", "9.9.9").with_artifact(MANIFEST_ARTIFACT),
            PackageId::new("d", "nodejs", "2.2.1").with_artifact("layer/icon.png"),
            PackageId::new("d", "nodejs", "2.2.1").with_artifact("starter-projects/nope.zip"),
            PackageId::new("d", "nope", "2.2.1").with_artifact(MANIFEST_ARTIFACT),
        ] {
            let err = c.resolve_metadata(&pkg).await.err().unwrap();
            assert!(matches!(err, CoreError::NotFound(_)), "{pkg}: {err}");
        }
    }

    #[tokio::test]
    async fn an_index_link_that_escapes_its_stack_is_refused() {
        let server = upstream().await;
        let pkg = PackageId::new("d", "evil", "1.0.0").with_artifact(MANIFEST_ARTIFACT);
        let err = client(&server).resolve_metadata(&pkg).await.err().unwrap();
        assert!(matches!(err, CoreError::Registry(_)), "{err}");
    }

    #[tokio::test]
    async fn a_starter_project_resolves_under_its_version() {
        let server = upstream().await;
        let pkg = PackageId::new("d", "nodejs", "latest")
            .with_artifact("starter-projects/nodejs-starter.zip");
        let meta = client(&server).resolve_metadata(&pkg).await.unwrap();
        assert!(meta
            .download_url
            .unwrap()
            .ends_with("/devfiles/nodejs/2.2.1/starter-projects/nodejs-starter"));
        assert!(meta.checksum.is_none());
    }

    #[tokio::test]
    async fn an_index_address_is_fetched_as_is_and_a_stack_as_its_entry() {
        let mut server = upstream().await;
        server
            .mock("GET", "/index/all?arch=amd64")
            .with_body(r#"[{"name":"nodejs","version":"2.2.1"}]"#)
            .create_async()
            .await;
        let c = client(&server);
        let doc = c
            .fetch_version_document("index/all?arch=amd64", DocumentKind::LEGACY_INDEX)
            .await
            .unwrap();
        assert_eq!(doc.body.as_json().unwrap()[0]["name"], "nodejs");
        let entry = c
            .fetch_version_document("nodejs", DocumentKind::Versions)
            .await
            .unwrap();
        assert_eq!(entry.body.as_json().unwrap()["name"], "nodejs");
        assert!(c
            .fetch_version_document("index/all?arch=amd64", DocumentKind::Versions)
            .await
            .is_err());
        assert!(c
            .fetch_version_document("../etc", DocumentKind::Versions)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn versions_are_listed_oldest_first() {
        let server = upstream().await;
        assert_eq!(
            client(&server).list_versions("nodejs").await.unwrap(),
            vec!["2.2.0", "2.2.1"]
        );
    }
}
