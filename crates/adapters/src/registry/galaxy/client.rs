//! `galaxy.ansible.com`'s collections API v3, and the v1 role surface beside it
//! (RFC 0031 §6.4).
//!
//! Coordinates (§4.3): the package is `{namespace}.{name}` — the spelling a
//! `requirements.yml` writes and an operator types — and one version is one
//! tarball:
//!
//! ```text
//! PackageId { name: "community.general", version: "13.4.0",
//!             artifact: Some("tarball") }
//!     → the version document's download_url, followed server-side
//! ```
//!
//! Four things this client does that a naive relay would not, each for a reason
//! read from `ansible-core`'s source rather than from the API documentation:
//!
//! - **Discovery is a probe, not a constant.** `g_connect` fetches the
//!   configured URL and retries it with `/api/` appended when the first answer
//!   is not a document with `available_versions`; the `v3`/`v1` path segments
//!   come out of that document because galaxy_ng answers `v3/` and a standalone
//!   pulp_ansible answers a longer path.
//! - **Listings are assembled, never relayed page by page** — see
//!   [`super::pagination`].
//! - **`download_url` is followed server-side**, through the SSRF guard and
//!   only to an origin this registry's own upstream published, including
//!   upstream's `302` to a token-signed content URL. The client drops
//!   `Authorization` across a redirect (`unredirected_headers`), so handing it
//!   one would hand it a `401`; and the bytes would never be cached.
//! - **The tarball is hashed while it streams** and compared with the version
//!   document's `artifact.sha256`. The client does the same check and fails on
//!   *"Mismatch artifact hash with downloaded file"*; doing it here means a
//!   corrupted fetch is never written into the cache for the next reader.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::galaxy::{
        self, address_of, artifact_filename, collapse_to_one_page, is_role, package_of,
        parse_collection, ROLE_PREFIX,
    },
};

use super::models::{Discovery, RoleSearch, VersionDetail};
use super::pagination::{collect_pages, PAGE_LIMIT};
use crate::registry::http_client::{
    basic_auth_get, cache_control, ensure_same_origin, fetch_json_document, new_http_client,
    no_redirect_client_pair, to_registry_error, UpstreamHttpOptions,
};
use crate::registry::ssrf::fetch_following_redirects_trusting;

/// How long one discovery document answers before it is re-probed.
///
/// `g_connect` caches it for the life of a client run; five minutes here is the
/// same trade `nodedist` makes for `index.tab` — a build farm installing twenty
/// collections probes once, and an upstream that gains or loses an API version
/// is noticed within a coffee break.
const DISCOVERY_TTL: Duration = Duration::from_secs(300);

/// The hosts a **role** archive may be fetched from, beside this registry's own
/// upstream (RFC 0031 decision 8).
///
/// Fixed rather than configurable: every role galaxy.ansible.com publishes
/// carries a `github.com` archive URL, and an allowlist nobody has needed yet
/// is a surface to get wrong. `role_download_hosts` is one field to add the day
/// a deployment asks for it.
pub const ROLE_DOWNLOAD_HOSTS: &[&str] = &["https://github.com"];

struct CachedDiscovery {
    fetched_at: Instant,
    parsed: Discovery,
    /// The API root that answered — the configured URL, or it with `/api/`
    /// appended.
    root: String,
}

pub struct GalaxyRegistryClient {
    http: reqwest::Client,
    /// Redirect-following pair for the artifact path: upstream answers
    /// `download_url` with a `302` to a token-signed content URL, and each hop
    /// has to be checked rather than followed by reqwest unseen.
    no_redirect: reqwest::Client,
    plain: reqwest::Client,
    basic_auth: Option<(String, String)>,
    configured: String,
    discovery: Mutex<Option<CachedDiscovery>>,
    /// Whether this registry serves the v1 role surface at all, and whether it
    /// fetches role bytes itself. `None` of the three modes changes the v3
    /// behaviour.
    serves_roles: bool,
    proxies_role_bytes: bool,
}

impl GalaxyRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let (no_redirect, plain) = no_redirect_client_pair(opts)?;
        Ok(Self {
            http: new_http_client(Some(5), opts)?,
            no_redirect,
            plain,
            basic_auth: opts.basic_auth.clone(),
            configured: base_url.into().trim_end_matches('/').to_owned(),
            discovery: Mutex::new(None),
            serves_roles: true,
            proxies_role_bytes: true,
        })
    }

    /// `roles = proxy | index | off` (§4.4), applied by the builder.
    pub fn with_roles(mut self, serves_roles: bool, proxies_role_bytes: bool) -> Self {
        self.serves_roles = serves_roles;
        self.proxies_role_bytes = proxies_role_bytes;
        self
    }

    /// The configured API root, for the handler's origin checks.
    pub fn base_url(&self) -> &str {
        &self.configured
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// The upstream's discovery document, probed the way `g_connect` probes it.
    ///
    /// An operator who writes `https://galaxy.ansible.com` gets the same
    /// registry as one who writes `https://galaxy.ansible.com/api/`, because
    /// the retry is the client's own behaviour and there is no reason for the
    /// proxy to be stricter than the thing it stands in for.
    async fn discovery(&self) -> Result<(String, Discovery), CoreError> {
        if let Some(cached) = self.discovery.lock().expect("discovery lock").as_ref() {
            if cached.fetched_at.elapsed() < DISCOVERY_TTL {
                return Ok((cached.root.clone(), cached.parsed.clone()));
            }
        }
        let mut last: Option<CoreError> = None;
        for root in [self.configured.clone(), format!("{}/api", self.configured)] {
            match self.probe_discovery(&root).await {
                Ok(parsed) => {
                    *self.discovery.lock().expect("discovery lock") = Some(CachedDiscovery {
                        fetched_at: Instant::now(),
                        parsed: parsed.clone(),
                        root: root.clone(),
                    });
                    return Ok((root, parsed));
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            CoreError::Registry(format!(
                "'{}' does not answer an Ansible Galaxy discovery document",
                self.configured
            ))
        }))
    }

    async fn probe_discovery(&self, root: &str) -> Result<Discovery, CoreError> {
        let url = format!("{root}/");
        let body: serde_json::Value = self
            .get(&url)
            .send()
            .await
            .map_err(to_registry_error)?
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(|e| CoreError::Registry(format!("parsing {url}: {e}")))?;
        serde_json::from_value(body).map_err(|e| {
            CoreError::Registry(format!("'{url}' is not a galaxy discovery document: {e}"))
        })
    }

    /// The base of one API version's endpoints, or a `NotSupported` naming the
    /// half this upstream does not serve.
    async fn api_base(&self, version: &str) -> Result<String, CoreError> {
        let (root, discovery) = self.discovery().await?;
        let segment = discovery.path_for(version).ok_or_else(|| {
            CoreError::NotSupported(format!(
                "'{}' advertises no '{version}' API: available_versions is {:?}",
                self.configured,
                discovery.available_versions.keys().collect::<Vec<_>>()
            ))
        })?;
        Ok(format!("{root}/{}", segment.trim_start_matches('/')))
    }

    /// The upstream URL of one collection's versions listing, first page.
    async fn versions_url(&self, namespace: &str, name: &str) -> Result<String, CoreError> {
        let v3 = self.api_base("v3").await?;
        Ok(format!(
            "{}/collections/{namespace}/{name}/versions/?limit={PAGE_LIMIT}",
            v3.trim_end_matches('/')
        ))
    }

    async fn collection_url(&self, namespace: &str, name: &str) -> Result<String, CoreError> {
        let v3 = self.api_base("v3").await?;
        Ok(format!(
            "{}/collections/{namespace}/{name}/",
            v3.trim_end_matches('/')
        ))
    }

    async fn version_url(
        &self,
        namespace: &str,
        name: &str,
        version: &str,
    ) -> Result<String, CoreError> {
        let v3 = self.api_base("v3").await?;
        Ok(format!(
            "{}/collections/{namespace}/{name}/versions/{version}/",
            v3.trim_end_matches('/')
        ))
    }

    /// The version document, typed enough to answer the resolve.
    async fn version_detail(&self, pkg: &PackageId) -> Result<VersionDetail, CoreError> {
        let (namespace, name) = parse_collection(&pkg.name)?;
        let url = self.version_url(namespace, name, &pkg.version).await?;
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "{} {} not found upstream",
                pkg.name, pkg.version
            )));
        }
        resp.error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(|e| CoreError::Registry(format!("parsing {url}: {e}")))
    }

    /// Follow a `download_url`, through the SSRF guard, refusing an origin this
    /// registry never published.
    ///
    /// The origin check is the half that survives a hostile *upstream*: a
    /// version document naming `http://169.254.169.254/…` is refused here
    /// before the guard ever has to see it, and one naming an arbitrary
    /// internet host is refused because this registry's bytes come from this
    /// registry's upstream.
    async fn fetch_url(
        &self,
        url: &str,
        trusted: &[String],
    ) -> Result<reqwest::Response, CoreError> {
        // `download_url` may be an **absolute path** rather than an absolute
        // URL: `_download_file` accepts either (it refuses only a value that is
        // neither, with *"Invalid non absolute download_url"*), galaxy_ng
        // behind a reverse proxy emits the path form, and a bare
        // `Url::parse` on one fails — so an origin check written against
        // `Url::parse` alone refuses every self-hosted upstream.
        //
        // Resolved against the **first trusted origin**, which is this
        // registry's own API root: a path is relative to the server that sent
        // it, and there is no other server it could mean.
        let url = &self.absolutise(url, trusted)?;
        let allowed = trusted
            .iter()
            .any(|base| ensure_same_origin(url, base).is_ok());
        if !allowed {
            return Err(CoreError::Registry(format!(
                "refusing to fetch '{url}': not an origin this registry serves from ({trusted:?})"
            )));
        }
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| CoreError::Registry(format!("invalid download URL '{url}': {e}")))?;
        fetch_following_redirects_trusting(
            &self.no_redirect,
            &self.plain,
            &self.basic_auth,
            trusted,
            parsed,
        )
        .await
    }

    /// An absolute-path `download_url`, resolved against the origin that sent
    /// it. An already-absolute URL is returned unchanged.
    fn absolutise(&self, url: &str, trusted: &[String]) -> Result<String, CoreError> {
        if reqwest::Url::parse(url).is_ok() {
            return Ok(url.to_owned());
        }
        if !url.starts_with('/') {
            return Err(CoreError::Registry(format!(
                "upstream gave a download_url that is neither absolute nor absolute-path: '{url}'"
            )));
        }
        let base = trusted.first().ok_or_else(|| {
            CoreError::Registry(
                "no trusted origin to resolve a relative download_url against".to_owned(),
            )
        })?;
        reqwest::Url::parse(base)
            .and_then(|b| b.join(url))
            .map(String::from)
            .map_err(|e| {
                CoreError::Registry(format!(
                    "resolving download_url '{url}' against '{base}': {e}"
                ))
            })
    }

    /// The origins a **collection** tarball may come from: this registry's
    /// upstream, and nothing else.
    async fn collection_origins(&self) -> Result<Vec<String>, CoreError> {
        let (root, _) = self.discovery().await?;
        Ok(vec![root, self.configured.clone()])
    }

    // --- the v1 role surface ------------------------------------------------

    async fn v1_base(&self) -> Result<String, CoreError> {
        if !self.serves_roles {
            return Err(CoreError::NotFound(
                "this registry does not serve the v1 role API".to_owned(),
            ));
        }
        self.api_base("v1").await
    }

    /// `v1/roles/?owner__username={user}&name={role}` — what
    /// `lookup_role_by_name` reads to turn `user.role` into the numeric id
    /// every later v1 request is addressed by.
    pub async fn role_search(
        &self,
        user: &str,
        role: &str,
    ) -> Result<serde_json::Value, CoreError> {
        let v1 = self.v1_base().await?;
        let url = format!(
            "{}/roles/?owner__username={}&name={}",
            v1.trim_end_matches('/'),
            crate::registry::http_client::percent_encode(user),
            crate::registry::http_client::percent_encode(role),
        );
        let doc = fetch_json_document(self.get(&url), &format!("role '{user}.{role}'")).await?;
        doc.body
            .as_json()
            .cloned()
            .ok_or_else(|| CoreError::Registry(format!("{url} did not answer JSON")))
    }

    /// `{user}.{role}` for a numeric role id, so a request addressed the way
    /// the client addresses it can still be blocked, cached and audited under
    /// the name a person wrote.
    pub async fn role_name_for_id(&self, id: &str) -> Result<String, CoreError> {
        let v1 = self.v1_base().await?;
        let url = format!("{}/roles/{id}/", v1.trim_end_matches('/'));
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("role {id} not found upstream")));
        }
        let body: serde_json::Value = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(|e| CoreError::Registry(format!("parsing {url}: {e}")))?;
        // `v1/roles/{id}/` answers the role object directly; a server that
        // answers a one-entry search instead is read the same way.
        let entry = if body.get("results").is_some() {
            let search: RoleSearch = serde_json::from_value(body)
                .map_err(|e| CoreError::Registry(format!("parsing {url}: {e}")))?;
            search.results.into_iter().next()
        } else {
            serde_json::from_value(body).ok()
        };
        let entry = entry.ok_or_else(|| CoreError::NotFound(format!("role {id} names nothing")))?;
        let (Some(user), Some(name)) = (entry.github_user, entry.name) else {
            return Err(CoreError::Registry(format!(
                "role {id} carries no github_user/name pair"
            )));
        };
        galaxy::role_key(&user, &name)
    }

    async fn role_versions_url(&self, id: &str) -> Result<String, CoreError> {
        let v1 = self.v1_base().await?;
        Ok(format!(
            "{}/roles/{id}/versions/?page_size={PAGE_LIMIT}",
            v1.trim_end_matches('/')
        ))
    }

    /// The origins a **role** archive may come from: this registry's upstream,
    /// plus the fixed allowlist of §7.
    async fn role_origins(&self) -> Result<Vec<String>, CoreError> {
        let mut origins = self.collection_origins().await?;
        origins.extend(ROLE_DOWNLOAD_HOSTS.iter().map(|h| (*h).to_owned()));
        Ok(origins)
    }
}

#[async_trait]
impl RegistryClient for GalaxyRegistryClient {
    fn registry_type(&self) -> &str {
        "galaxy"
    }

    /// The version document's `created_at`, `artifact.sha256` and
    /// `download_url`.
    ///
    /// `published_at` is `created_at`, an RFC 3339 instant, so the age gate
    /// needs no midnight rule. `download_url` travels on the metadata so
    /// [`Self::fetch_artifact_resolved`] can use it without reading the
    /// document a second time.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        if is_role(&pkg.name) {
            return self.resolve_role_metadata(pkg).await;
        }
        let detail = self.version_detail(pkg).await?;
        let sha256 = detail.artifact.as_ref().and_then(|a| a.sha256.clone());
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: detail
                .created_at
                .as_deref()
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            download_url: detail.download_url.clone(),
            // **Bare hex, not `sha256:<hex>`.** `integrity::parse_expected`
            // accepts an SRI `<algo>-<base64>` token or a bare hex digest whose
            // algorithm it infers from the length, and nothing else — a
            // prefixed value parses as neither, so the cache-write verification
            // logs "advertised checksum could not be parsed; skipping
            // verification" and silently does not run. Found by the real client
            // in `tests/heavy/galaxy.sh`: the install succeeded either way,
            // because `ansible-galaxy` does its own check, so nothing but that
            // log line said the server's had stopped (RFC 0031 §13).
            checksum: sha256
                .clone()
                .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())),
            // Upstream `signatures` are relayed as served; nothing here mints
            // or verifies one, so this is not a claim either way (§3).
            is_signed: None,
            extra: serde_json::json!({
                "requires_ansible": detail.requires_ansible,
                "sha256": sha256,
                "size": detail.artifact.as_ref().and_then(|a| a.size),
                "filename": detail.artifact.as_ref().and_then(|a| a.filename.clone()),
            }),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let resolved = self.resolve_metadata(pkg).await?;
        self.fetch_artifact_resolved(pkg, &resolved)
            .await?
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "{} {} carries no download_url upstream",
                    pkg.name, pkg.version
                ))
            })
    }

    /// The fetch for a caller that already read the version document.
    ///
    /// Re-applies every check [`Self::fetch_artifact`] would: the URL came from
    /// *upstream's* document, and "our own document said so" is exactly the
    /// position the Open VSX and Terraform cross-host bugs started from.
    async fn fetch_artifact_resolved(
        &self,
        pkg: &PackageId,
        resolved: &PackageMetadata,
    ) -> Result<Option<FetchedArtifact>, CoreError> {
        let Some(url) = resolved.download_url.as_deref() else {
            return Ok(None);
        };
        let trusted = if is_role(&pkg.name) {
            if !self.proxies_role_bytes {
                return Err(CoreError::NotSupported(
                    "roles = \"index\": this registry proxies role metadata and not role bytes"
                        .to_owned(),
                ));
            }
            self.role_origins().await?
        } else {
            self.collection_origins().await?
        };
        let response = self.fetch_url(url, &trusted).await?;
        let cache_control = cache_control(&response);
        let stream = response.bytes_stream().map_err(to_registry_error);
        Ok(Some(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        }))
    }

    /// Every version of a collection, oldest-first, from the assembled listing.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let package = package_of(package);
        if is_role(package) {
            return Ok(Vec::new());
        }
        let (namespace, name) = parse_collection(package)?;
        let start = self.versions_url(namespace, name).await?;
        let (entries, _) = self.walk(&start).await?;
        let mut versions: Vec<String> = entries
            .iter()
            .filter_map(|e| e.get("version")?.as_str().map(str::to_owned))
            .collect();
        // Oldest-first, which is what `list_versions` promises; the shared
        // comparator sorts newest-first, so the sort is reversed.
        versions.sort_by(|a, b| batlehub_core::services::version_order::newest_first(b, a));
        Ok(versions)
    }

    /// The four documents of §4.4, each assembled into exactly one page.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        match kind {
            DocumentKind::Versions => {
                let (namespace, name) = parse_collection(package_of(package))?;
                let start = self.versions_url(namespace, name).await?;
                let (entries, _) = self.walk(&start).await?;
                let mut doc = serde_json::json!({ "data": entries });
                collapse_to_one_page(&mut doc);
                Ok(VersionDocument::json(doc))
            }
            DocumentKind::COLLECTION => {
                let (namespace, name) = parse_collection(package_of(package))?;
                let url = self.collection_url(namespace, name).await?;
                fetch_json_document(self.get(&url), &format!("collection '{package}'")).await
            }
            DocumentKind::VERSION_DETAIL => {
                let name = package_of(package);
                let version = address_of(package).ok_or_else(|| {
                    CoreError::NotSupported(format!(
                        "'{package}' names no version; a version document is addressed as \
                         '{{namespace}}.{{name}}@{{version}}'"
                    ))
                })?;
                let (namespace, collection) = parse_collection(name)?;
                let url = self.version_url(namespace, collection, version).await?;
                fetch_json_document(self.get(&url), &format!("{name} {version}")).await
            }
            DocumentKind::ROLE_VERSIONS => {
                let id = address_of(package).ok_or_else(|| {
                    CoreError::NotSupported(format!(
                        "'{package}' names no role id; a role listing is addressed as \
                         '{ROLE_PREFIX}{{user}}.{{role}}@{{id}}'"
                    ))
                })?;
                let start = self.role_versions_url(id).await?;
                let (entries, _) = self.walk(&start).await?;
                let mut doc = serde_json::json!({ "results": entries });
                batlehub_core::services::blocking::galaxy::collapse_role_page(&mut doc);
                Ok(VersionDocument::json(doc))
            }
            DocumentKind::ROLE => {
                let name = package_of(package).trim_start_matches(ROLE_PREFIX);
                let (user, role) = name.split_once('.').ok_or_else(|| {
                    CoreError::NotSupported(format!("'{package}' is not '{{user}}.{{role}}'"))
                })?;
                let value = self.role_search(user, role).await?;
                Ok(VersionDocument::json(value))
            }
            other => Err(CoreError::NotSupported(format!(
                "Ansible Galaxy has no '{other}' listing document"
            ))),
        }
    }

    /// `roles/#{id}` → `roles/{user}.{role}` (RFC 0031 §6.2).
    ///
    /// `ansible-galaxy` learns a role's numeric id from the search document and
    /// addresses every later v1 request by it, so the second request carries
    /// nothing a person wrote. Resolved here, before the funnel, so the cache,
    /// the rules and the block list all see one coordinate per role — the
    /// arrangement the JetBrains Marketplace's numeric update ids already use.
    ///
    /// `Ok(None)` for every other coordinate, which costs nothing.
    async fn canonical_coordinate(&self, pkg: &PackageId) -> Result<Option<PackageId>, CoreError> {
        let Some(id) = batlehub_core::services::galaxy::role_id_of(&pkg.name) else {
            return Ok(None);
        };
        let name = self.role_name_for_id(id).await?;
        Ok(Some(PackageId {
            name,
            ..pkg.clone()
        }))
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<batlehub_core::ports::UpstreamPackage>, CoreError> {
        // Galaxy's search API is a non-goal (§3): `registry search` answers from
        // what this instance holds, as it does for every kind whose upstream
        // search is not proxied.
        let _ = (query, limit);
        Ok(Vec::new())
    }
}

impl GalaxyRegistryClient {
    /// [`collect_pages`] with this client's credentials attached.
    async fn walk(&self, start: &str) -> Result<(Vec<serde_json::Value>, Option<u64>), CoreError> {
        let http = self.http.clone();
        let auth = self.basic_auth.clone();
        collect_pages(
            move |url| {
                let http = http.clone();
                let auth = auth.clone();
                Box::pin(async move {
                    basic_auth_get(&http, &auth, &url)
                        .send()
                        .await
                        .map_err(to_registry_error)
                })
            },
            start,
        )
        .await
    }

    /// A role version's metadata: the `download_url` its listing entry carries.
    ///
    /// Resolved from the listing rather than from a per-version endpoint
    /// because v1 has none — `Role.install` reads the matching entry of
    /// `v1/roles/{id}/versions/` and takes its `download_url`, preferring it
    /// over the `github.com/{user}/{repo}/archive/{version}.tar.gz` URL it
    /// would otherwise build itself.
    async fn resolve_role_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let name = pkg.name.trim_start_matches(ROLE_PREFIX);
        let (user, role) = name.split_once('.').ok_or_else(|| {
            CoreError::NotSupported(format!("'{}' is not '{{user}}.{{role}}'", pkg.name))
        })?;
        let search = self.role_search(user, role).await?;
        let id = search
            .get("results")
            .and_then(|r| r.get(0))
            .and_then(|r| r.get("id"))
            .ok_or_else(|| CoreError::NotFound(format!("role '{name}' not found upstream")))?;
        let id = match id {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => {
                return Err(CoreError::Registry(format!(
                    "role '{name}' has a non-scalar id: {other}"
                )))
            }
        };
        let start = self.role_versions_url(&id).await?;
        let (entries, _) = self.walk(&start).await?;
        let entry = entries
            .iter()
            .find(|e| {
                let v = e
                    .get("version")
                    .and_then(|v| v.as_str())
                    .or_else(|| e.get("name").and_then(|v| v.as_str()));
                v == Some(pkg.version.as_str())
            })
            .ok_or_else(|| {
                CoreError::NotFound(format!("role '{name}' has no version {}", pkg.version))
            })?;
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: entry
                .get("release_date")
                .and_then(|v| v.as_str())
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc)),
            download_url: entry
                .get("download_url")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({ "role_id": id }),
            cache_control: None,
        })
    }

    /// The upstream filename for a collection coordinate, for callers that
    /// build a served path rather than a storage key.
    pub fn artifact_name(package: &str, version: &str) -> Result<String, CoreError> {
        let (namespace, name) = parse_collection(package)?;
        Ok(artifact_filename(namespace, name, version))
    }
}
