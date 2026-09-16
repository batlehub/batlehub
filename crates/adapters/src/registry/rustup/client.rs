//! The `static.rust-lang.org` tree, read as a registry (RFC 0024 §6.4).
//!
//! Two subtrees under one root. `dist/` holds the channel manifests and, under
//! a directory per release date, one tarball per component per target;
//! `rustup/` holds the installer's own releases. `manifests.txt` at the root
//! lists every manifest ever published, and is the only document that maps a
//! stable release to the date its files live under.
//!
//! Coordinates (§4.3): one package `rust`, whose versions are rustup's own
//! toolchain names, and one package `rustup` for the installer:
//!
//! ```text
//! PackageId { name: "rust", version: "1.98.1",
//!             artifact: Some("rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz") }
//!     → GET {base}/dist/2026-09-03/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz
//! PackageId { name: "rustup", version: "1.29.1",
//!             artifact: Some("x86_64-unknown-linux-gnu/rustup-init") }
//!     → GET {base}/rustup/archive/1.29.1/x86_64-unknown-linux-gnu/rustup-init
//! ```
//!
//! The date in the first URL is not in the coordinate: a release's files are
//! the same bytes whichever dated directory served them, so one cache key per
//! release and file is the right key. `nightly-2026-09-05` carries its date in
//! the version; a stable release's date is read from `manifests.txt`, which is
//! fetched once and kept for [`MANIFESTS_TTL`].
//!
//! No redirect chain: the tree serves its own bytes.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::rustup::{
        channel_of, date_of_version, parse_release_date, ManifestsTxt, RUSTUP_PACKAGE, RUST_PACKAGE,
    },
};

use super::models::ReleaseStable;
use crate::registry::http_client::{
    basic_auth_get, cache_control, fetch_text_document, new_http_client, to_registry_error,
    UpstreamHttpOptions,
};

/// How long one fetched `manifests.txt` answers a date lookup before it is
/// re-read.
///
/// The same five minutes `nodedist` keeps `index.tab` for, and for the same
/// reason: a build farm installing one toolchain reads the file once rather
/// than once per component tarball. The *proxied* document has its own cache in
/// `ProxyService`, keyed by the registry's `metadata_ttl`; this one is only for
/// recovering a stable release's date.
const MANIFESTS_TTL: Duration = Duration::from_secs(300);

struct CachedManifests {
    fetched_at: Instant,
    parsed: ManifestsTxt,
}

pub struct RustupRegistryClient {
    http: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
    manifests: Mutex<Option<CachedManifests>>,
}

impl RustupRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(5), opts)?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            basic_auth: opts.basic_auth.clone(),
            manifests: Mutex::new(None),
        })
    }

    /// The tree root, for the handler's URL normalisation (§4.4).
    pub fn base_url(&self) -> &str {
        &self.base_url
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

    /// `{base}/dist/channel-rust-{name}.toml`, or its dated form when the
    /// channel string carries a directory (`2026-09-05/nightly`).
    fn manifest_url(&self, channel: &str) -> String {
        match channel.split_once('/') {
            Some((date, name)) => {
                format!("{}/dist/{date}/channel-rust-{name}.toml", self.base_url)
            }
            None => format!("{}/dist/channel-rust-{channel}.toml", self.base_url),
        }
    }

    /// `manifests.txt`, from the in-process copy when it is fresh.
    async fn manifests(&self) -> Result<ManifestsTxt, CoreError> {
        if let Some(cached) = self.manifests.lock().expect("manifests lock").as_ref() {
            if cached.fetched_at.elapsed() < MANIFESTS_TTL {
                return Ok(cached.parsed.clone());
            }
        }
        let url = format!("{}/manifests.txt", self.base_url);
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        let text = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .text()
            .await
            .map_err(to_registry_error)?;
        let parsed = ManifestsTxt::parse(&text);
        *self.manifests.lock().expect("manifests lock") = Some(CachedManifests {
            fetched_at: Instant::now(),
            parsed: parsed.clone(),
        });
        Ok(parsed)
    }

    /// The dated directory a release's files live under.
    ///
    /// Free for a dated channel — the version carries it — and one cached
    /// `manifests.txt` lookup for a stable release. The **newest** row wins
    /// when a version was published more than once, because a re-publish
    /// supersedes.
    async fn date_for(&self, version: &str) -> Result<String, CoreError> {
        if let Some(date) = date_of_version(version) {
            return Ok(date.to_owned());
        }
        let manifests = self.manifests().await?;
        manifests
            .rows
            .iter()
            .rev()
            .find(|r| r.coordinate.as_deref() == Some(version))
            .map(|r| r.date.clone())
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "Rust release '{version}' is not named by manifests.txt"
                ))
            })
    }

    /// The upstream URL for one coordinate's artifact.
    async fn artifact_url(&self, pkg: &PackageId) -> Result<String, CoreError> {
        let Some(artifact) = pkg.artifact.as_deref() else {
            return Err(CoreError::NotFound(format!(
                "a Rust release is a set of files; name one under {}",
                pkg.version
            )));
        };
        if pkg.name == RUSTUP_PACKAGE {
            return Ok(format!(
                "{}/rustup/archive/{}/{artifact}",
                self.base_url, pkg.version
            ));
        }
        let date = self.date_for(&pkg.version).await?;
        Ok(format!("{}/dist/{date}/{artifact}", self.base_url))
    }

    /// Whether an artifact exists upstream, without reading a 200 MB tarball to
    /// answer a yes/no question.
    async fn exists(&self, url: &str) -> Result<bool, CoreError> {
        let resp = self.head(url).send().await.map_err(to_registry_error)?;
        match resp.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s if s.is_success() => Ok(true),
            s => Err(CoreError::Registry(format!(
                "HEAD {url} returned {s} from upstream"
            ))),
        }
    }

    /// The installer's current version, from `rustup/release-stable.toml`.
    pub async fn rustup_release_version(&self) -> Result<String, CoreError> {
        let url = format!("{}/rustup/release-stable.toml", self.base_url);
        let text = self
            .get(&url)
            .send()
            .await
            .map_err(to_registry_error)?
            .error_for_status()
            .map_err(to_registry_error)?
            .text()
            .await
            .map_err(to_registry_error)?;
        let release: ReleaseStable = toml::from_str(&text).map_err(|e| {
            CoreError::Registry(format!("rustup/release-stable.toml did not parse: {e}"))
        })?;
        Ok(release.version)
    }
}

#[async_trait]
impl RegistryClient for RustupRegistryClient {
    fn registry_type(&self) -> &str {
        "rustup"
    }

    /// The release date, and a `HEAD` on the file to confirm the coordinate.
    ///
    /// A `rust` coordinate always has a date — the directory *is* the date — so
    /// the age gate has one for every toolchain. A `rustup` coordinate never
    /// does: the installer's tree publishes no dates, which is the case
    /// `deny_missing_timestamp` exists to decide and why config validation
    /// makes an operator state it on this kind (§6.7).
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let url = self.artifact_url(pkg).await?;
        if !self.exists(&url).await? {
            return Err(CoreError::NotFound(format!(
                "{} {} ({}) not found upstream",
                pkg.name,
                pkg.version,
                pkg.artifact.as_deref().unwrap_or("")
            )));
        }
        let (published_at, extra) = if pkg.name == RUSTUP_PACKAGE {
            (None, serde_json::json!({ "date": null }))
        } else {
            let date = self.date_for(&pkg.version).await?;
            (
                parse_release_date(&date),
                serde_json::json!({ "date": date }),
            )
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
        let url = self.artifact_url(pkg).await?;
        tracing::debug!(url = %url, "fetching Rust dist file");

        let response = self.get(&url).send().await.map_err(to_registry_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{url} not found upstream")));
        }
        let response = response.error_for_status().map_err(to_registry_error)?;
        let cache_control = cache_control(&response);
        let stream = response.bytes_stream().map_err(to_registry_error);
        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    /// Four documents, three of them text the client reads as-is.
    ///
    /// A channel manifest is keyed by the channel the listing package string
    /// carries (`rust/stable`, `rust/2026-09-05/nightly`), so two channels are
    /// two cache entries. Upstream serves every file in this tree as
    /// `binary/octet-stream`; these are served as text because rustup ignores
    /// the type and a browser is better off with the document it is.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        const TOML: &str = "text/plain; charset=utf-8";
        match kind {
            DocumentKind::MANIFEST => {
                let channel = channel_of(package).ok_or_else(|| {
                    CoreError::NotSupported(format!(
                        "'{package}' names no channel; a manifest is addressed as \
                         '{RUST_PACKAGE}/{{channel}}'"
                    ))
                })?;
                let url = self.manifest_url(channel);
                fetch_text_document(self.get(&url), &format!("manifest for '{channel}'"), TOML)
                    .await
            }
            DocumentKind::MANIFEST_ASC => {
                let channel = channel_of(package).ok_or_else(|| {
                    CoreError::NotSupported(format!(
                        "'{package}' names no channel; a signature is addressed as \
                         '{RUST_PACKAGE}/{{channel}}'"
                    ))
                })?;
                let url = format!("{}.asc", self.manifest_url(channel));
                fetch_text_document(
                    self.get(&url),
                    &format!("manifest signature for '{channel}'"),
                    "application/pgp-signature",
                )
                .await
            }
            DocumentKind::Versions => {
                let url = format!("{}/manifests.txt", self.base_url);
                fetch_text_document(self.get(&url), "manifests.txt", TOML).await
            }
            DocumentKind::STABLE_DATE => {
                let url = format!("{}/dist/channel-rust-stable-date.txt", self.base_url);
                fetch_text_document(self.get(&url), "channel-rust-stable-date.txt", TOML).await
            }
            DocumentKind::RUSTUP_RELEASE => {
                let url = format!("{}/rustup/release-stable.toml", self.base_url);
                fetch_text_document(self.get(&url), "rustup/release-stable.toml", TOML).await
            }
            other => Err(CoreError::NotSupported(format!(
                "the Rust dist tree has no '{other}' listing document"
            ))),
        }
    }

    /// Every release `manifests.txt` names, oldest first, in the coordinate
    /// spelling — or the one version the installer's tree currently has.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        if package == RUSTUP_PACKAGE {
            return Ok(vec![self.rustup_release_version().await?]);
        }
        Ok(self.manifests().await?.versions())
    }
}
