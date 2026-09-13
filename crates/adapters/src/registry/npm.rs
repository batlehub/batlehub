use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;
use serde::Deserialize;
use std::collections::HashMap;

use batlehub_core::{
    entities::{MetadataLinks, MetadataReadme, PackageId, PackageMetadata, ReadmeFormat},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
};

use super::http_client::{
    basic_auth_get, cache_control, ensure_same_origin, new_http_client, percent_encode,
    to_registry_error, UpstreamHttpOptions,
};

/// npm registry client (registry.npmjs.org or compatible).
///
/// Supported `PackageId` conventions:
/// - `version = "latest"` or a dist-tag  → resolve via packument, return metadata
/// - `version = "1.2.3"` (exact semver)  → version-specific metadata
/// - `artifact = Some("tarball")`        → stream the `.tgz` for that version
pub struct NpmRegistryClient {
    http: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
}

impl NpmRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(None, opts)?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            basic_auth: opts.basic_auth.clone(),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct NpmPackument {
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    versions: HashMap<String, NpmVersionMeta>,
    /// Per-version publish timestamps from the full packument.
    /// Keys are version strings (e.g. `"1.2.3"`) plus the special keys
    /// `"created"` and `"modified"`. Only present in the full packument
    /// (`application/json`); absent in the abbreviated install manifest.
    #[serde(default)]
    time: HashMap<String, String>,
    /// The package's README, at the document root.
    ///
    /// Package-level, not per-version: npm carries both, and this one describes
    /// whatever `dist-tags.latest` currently points at. It is attributed to that
    /// version and to no other (RFC 0007, decision 6) — inventing a per-version
    /// claim from a package-level field would be a guess presented as a fact.
    #[serde(default)]
    readme: Option<String>,
    /// The package's repository, at the document root — the fallback when the
    /// version's own entry omits it, which is common for older publishes.
    #[serde(default, deserialize_with = "lenient_repository")]
    repository: Option<NpmRepository>,
    #[serde(default, deserialize_with = "lenient_homepage")]
    homepage: Option<String>,
}

/// npm spells this two ways and both are in the wild: a bare string (often the
/// `github:user/repo` shorthand) or `{ "type": "git", "url": "git+https://…" }`.
/// `MetadataLinks` untangles the spelling; this only has to accept both shapes.
///
/// Built by [`lenient_repository`] rather than derived, for the reason given
/// there: a third spelling exists in the wild and it must not fail a document.
#[derive(Debug)]
enum NpmRepository {
    Url(String),
    Object { url: Option<String> },
}

impl NpmRepository {
    fn url(&self) -> Option<&str> {
        match self {
            Self::Url(url) => Some(url),
            Self::Object { url } => url.as_deref(),
        }
    }
}

/// `repository`, read for whatever it turns out to be.
///
/// A packument is not a schema: every version entry is the `package.json` that
/// was published with it, including shapes npm itself stopped accepting years
/// ago. `tmp@0.0.4` (2012) spells `repository` as a one-element *array* of the
/// object form, and `fs-extra@0.0.1`, `jsonfile@0.0.1` and their siblings spell
/// `homepage` as a one-element array of the string.
///
/// serde reads the whole document, so one such entry refused the *package* —
/// `data did not match any variant of untagged enum NpmRepository`, surfaced to
/// the client as `502 malformed npm packument`. Every version of `tmp`,
/// `fs-extra` and `jsonfile` was un-installable through this proxy, which is
/// how `closed_world.sh`'s ovsx phase found it.
///
/// Both fields feed the links on a metadata page and nothing else, so a shape
/// neither reader understands is dropped — `None`, the same as absent. A
/// document a client asked for is never failed for a field no client reads.
fn lenient_repository<'de, D>(de: D) -> Result<Option<NpmRepository>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<serde_json::Value>::deserialize(de)?.and_then(repository_of))
}

fn repository_of(value: serde_json::Value) -> Option<NpmRepository> {
    match value {
        serde_json::Value::String(url) => Some(NpmRepository::Url(url)),
        serde_json::Value::Object(mut fields) => Some(NpmRepository::Object {
            url: match fields.remove("url") {
                Some(serde_json::Value::String(url)) => Some(url),
                _ => None,
            },
        }),
        // The array spelling: the first entry that yields one, as npm's own
        // readers do — a later entry is a mirror of the same repository.
        serde_json::Value::Array(entries) => entries.into_iter().find_map(repository_of),
        _ => None,
    }
}

/// `homepage`, read the same way and for the same reason as [`lenient_repository`].
fn lenient_homepage<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<serde_json::Value>::deserialize(de)?.and_then(homepage_of))
}

fn homepage_of(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(url) => Some(url).filter(|url| !url.is_empty()),
        serde_json::Value::Array(entries) => entries.into_iter().find_map(homepage_of),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct NpmVersionMeta {
    #[allow(dead_code)]
    version: String,
    dist: NpmDist,
    #[serde(rename = "_npmUser")]
    npm_user: Option<NpmUser>,
    /// This version's own README, when the packument carries one.
    ///
    /// Often absent or the placeholder `"ERROR: No README data found!"` for
    /// versions published by tooling that only wrote the tarball — which is why
    /// npm's `readme_support()` is `MetadataThenArchive` and not `Metadata`.
    #[serde(default)]
    readme: Option<String>,
    /// This version's own repository. Preferred over the document root's: a
    /// package that moved forge between releases named the old one in the old
    /// version, and that is the honest answer for *that* version.
    #[serde(default, deserialize_with = "lenient_repository")]
    repository: Option<NpmRepository>,
    #[serde(default, deserialize_with = "lenient_homepage")]
    homepage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NpmDist {
    tarball: String,
    #[serde(default)]
    integrity: String,
    #[serde(default)]
    shasum: String,
}

#[derive(Debug, Deserialize)]
struct NpmUser {
    name: Option<String>,
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for NpmRegistryClient {
    fn registry_type(&self) -> &str {
        "npm"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let packument = self.fetch_packument(&pkg.name).await?;

        // Resolve dist-tag (e.g. "latest") → concrete version string.
        let resolved_version = resolve_dist_tag(&packument.dist_tags, &pkg.version).to_owned();

        let version_meta = packument.versions.get(&resolved_version).ok_or_else(|| {
            CoreError::NotFound(format!(
                "npm package {}@{} not found",
                pkg.name, resolved_version
            ))
        })?;

        let download_url = if pkg.artifact.as_deref() == Some("tarball") {
            Some(version_meta.dist.tarball.clone())
        } else {
            None
        };

        let checksum = pick_checksum(&version_meta.dist);

        let extra = serde_json::json!({
            "resolved_version": resolved_version,
            "tarball": version_meta.dist.tarball,
            "publisher": version_meta.npm_user.as_ref().and_then(|u| u.name.as_deref()),
            "readme": packument_readme(&packument, version_meta, &resolved_version),
            // The version's own, falling back to the document root's.
            "links": MetadataLinks::new(
                version_meta
                    .repository
                    .as_ref()
                    .or(packument.repository.as_ref())
                    .and_then(NpmRepository::url),
                version_meta
                    .homepage
                    .as_deref()
                    .or(packument.homepage.as_deref()),
            ),
        });

        let published_at = packument
            .time
            .get(&resolved_version)
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        Ok(PackageMetadata {
            id: PackageId {
                version: resolved_version,
                ..pkg.clone()
            },
            published_at,
            download_url,
            checksum,
            is_signed: None,
            extra,
            cache_control: None,
        })
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let packument = self.fetch_packument(package).await?;
        let mut versions: Vec<String> = packument.versions.into_keys().collect();
        // Sort by semver lexicographically as a best-effort ordering (oldest first).
        versions.sort();
        Ok(versions)
    }

    /// The packument, deserialized as untyped JSON rather than through
    /// [`NpmPackument`].
    ///
    /// Deliberate: the typed struct keeps only the four fields the rule engine
    /// needs, and a client resolving against this document needs all the rest —
    /// `dependencies`, `engines`, `deprecated`, whatever npm adds next. Round-
    /// tripping through `Value` preserves fields this proxy has never heard of,
    /// which is the difference between a usable packument and a lossy one.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "npm has no '{kind}' listing document"
            )));
        }
        Ok(VersionDocument::json(
            self.fetch_packument_json(package).await?,
        ))
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        // Resolve the tarball URL for this version.
        let packument = self.fetch_packument(&pkg.name).await?;
        let resolved_version = resolve_dist_tag(&packument.dist_tags, &pkg.version).to_owned();

        let version_meta = packument.versions.get(&resolved_version).ok_or_else(|| {
            CoreError::NotFound(format!(
                "npm package {}@{} not found",
                pkg.name, resolved_version
            ))
        })?;

        let tarball_url = &version_meta.dist.tarball;
        ensure_same_origin(tarball_url, &self.base_url)?;
        tracing::debug!(url = %tarball_url, "fetching npm tarball");

        let response = self
            .get(tarball_url)
            .send()
            .await
            .map_err(to_registry_error)?
            .error_for_status()
            .map_err(to_registry_error)?;

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
        #[derive(Deserialize)]
        struct SearchResponse {
            objects: Vec<SearchObject>,
        }
        #[derive(Deserialize)]
        struct SearchObject {
            package: SearchPackage,
        }
        #[derive(Deserialize)]
        struct SearchPackage {
            name: String,
            version: String,
            description: Option<String>,
        }

        let url = format!(
            "{}/-/v1/search?text={}&size={}",
            self.base_url,
            percent_encode(query),
            limit.min(50),
        );
        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: SearchResponse = res.json().await.map_err(to_registry_error)?;

        Ok(body
            .objects
            .into_iter()
            .map(|o| UpstreamPackage {
                name: o.package.name,
                latest_version: o.package.version,
                description: o.package.description,
            })
            .collect())
    }
}

fn encode_npm_name(name: &str) -> String {
    name.replace('/', "%2F")
}

/// Resolve a dist-tag (e.g. `"latest"`) to a concrete version string.
/// Returns the version unchanged when the tag is not in `dist_tags`.
fn resolve_dist_tag<'a>(dist_tags: &'a HashMap<String, String>, version: &'a str) -> &'a str {
    dist_tags
        .get(version)
        .map(String::as_str)
        .unwrap_or(version)
}

/// npm's own placeholder for "the tarball had no README".
///
/// It is a string, so a naïve `Option::is_some` would store it and the console
/// would render an error message as documentation.
const NPM_MISSING_README: &str = "ERROR: No README data found!";

/// The README to attribute to `resolved_version`, from a packument.
///
/// Two sources, in order: the version's own `readme`, and — only when this
/// version is the one `dist-tags.latest` names — the document root's, flagged
/// as package-level so the panel can say which it is showing. There is no third
/// case: attributing the root README to an arbitrary version would show `2.x`'s
/// API to a `1.x` reader, which is the failure RFC 0007 §2.4 is about.
///
/// Returns `None` when neither carries one; npm's tarball still might, and
/// `readme_support()` says `MetadataThenArchive` for exactly that reason.
fn packument_readme(
    packument: &NpmPackument,
    version_meta: &NpmVersionMeta,
    resolved_version: &str,
) -> Option<MetadataReadme> {
    let usable = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty() && *t != NPM_MISSING_README)
            .map(str::to_owned)
    };

    if let Some(text) = usable(&version_meta.readme) {
        return Some(MetadataReadme::text(text, ReadmeFormat::Markdown));
    }
    if packument.dist_tags.get("latest").map(String::as_str) == Some(resolved_version) {
        if let Some(text) = usable(&packument.readme) {
            return Some(MetadataReadme::text(text, ReadmeFormat::Markdown).package_level());
        }
    }
    None
}

/// Select the best checksum available: `integrity` (preferred) over `shasum`.
fn pick_checksum(dist: &NpmDist) -> Option<String> {
    if !dist.integrity.is_empty() {
        Some(dist.integrity.clone())
    } else if !dist.shasum.is_empty() {
        Some(dist.shasum.clone())
    } else {
        None
    }
}

impl NpmRegistryClient {
    async fn fetch_packument(&self, name: &str) -> Result<NpmPackument, CoreError> {
        let doc = self.fetch_packument_json(name).await?;
        serde_json::from_value(doc)
            .map_err(|e| CoreError::Registry(format!("malformed npm packument for {name}: {e}")))
    }

    /// The upstream packument as raw JSON. Both the typed read above and
    /// [`RegistryClient::fetch_version_document`] go through here so the request
    /// shape — encoding, `Accept`, 404 handling — is stated once.
    async fn fetch_packument_json(&self, name: &str) -> Result<serde_json::Value, CoreError> {
        // Scoped packages: @scope/pkg → must be percent-encoded as @scope%2Fpkg
        let encoded = encode_npm_name(name);
        let url = format!("{}/{}", self.base_url, encoded);

        let resp = self
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("npm package {name} not found")));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<serde_json::Value>()
            .await
            .map_err(to_registry_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three historic spellings that used to refuse the whole package, as
    /// `registry.npmjs.org` still serves them: `tmp@0.0.4`'s array repository,
    /// `fs-extra@0.0.1`'s array homepage, and the modern object/string forms
    /// beside them so the lenient reader is not a looser reader.
    #[test]
    fn packument_survives_the_historic_field_spellings() {
        let doc = serde_json::json!({
            "dist-tags": { "latest": "1.0.0" },
            "repository": [{ "type": "git", "url": "git://github.com/raszi/tmp.git" }],
            "homepage": ["https://github.com/jprichardson/node-fs-extra"],
            "versions": {
                "0.0.4": {
                    "version": "0.0.4",
                    "dist": { "tarball": "https://example.com/t-0.0.4.tgz" },
                    "repository": [{ "type": "git", "url": "git://github.com/raszi/tmp.git" }],
                    "homepage": [""]
                },
                "1.0.0": {
                    "version": "1.0.0",
                    "dist": { "tarball": "https://example.com/t-1.0.0.tgz" },
                    "repository": { "type": "git", "url": "git+https://github.com/raszi/node-tmp.git" },
                    "homepage": "http://github.com/raszi/node-tmp"
                }
            }
        });

        let packument: NpmPackument = serde_json::from_value(doc)
            .expect("the historic spellings must not refuse the package");

        assert_eq!(
            packument.repository.as_ref().and_then(NpmRepository::url),
            Some("git://github.com/raszi/tmp.git"),
            "the array spelling is read, not dropped"
        );
        assert_eq!(
            packument.homepage.as_deref(),
            Some("https://github.com/jprichardson/node-fs-extra")
        );

        let old = &packument.versions["0.0.4"];
        assert_eq!(
            old.repository.as_ref().and_then(NpmRepository::url),
            Some("git://github.com/raszi/tmp.git")
        );
        // `[""]` carries no link: absent rather than an empty one.
        assert_eq!(old.homepage, None);

        let new = &packument.versions["1.0.0"];
        assert_eq!(
            new.repository.as_ref().and_then(NpmRepository::url),
            Some("git+https://github.com/raszi/node-tmp.git")
        );
        assert_eq!(
            new.homepage.as_deref(),
            Some("http://github.com/raszi/node-tmp")
        );
    }

    /// A shape no reader understands is dropped, not fatal: the document is
    /// what the client asked for, and neither field is in it.
    #[test]
    fn packument_drops_unreadable_field_shapes() {
        let doc = serde_json::json!({
            "dist-tags": {},
            "repository": 42,
            "homepage": { "url": "https://example.com" },
            "versions": {}
        });

        let packument: NpmPackument = serde_json::from_value(doc).expect("still a packument");
        assert!(packument.repository.is_none());
        assert!(packument.homepage.is_none());
    }

    /// The string spelling of `repository`, and an object that has no `url`.
    #[test]
    fn repository_string_and_urlless_object() {
        assert_eq!(
            repository_of(serde_json::json!("github:user/repo"))
                .as_ref()
                .and_then(NpmRepository::url),
            Some("github:user/repo")
        );
        assert_eq!(
            repository_of(serde_json::json!({ "type": "git" }))
                .as_ref()
                .and_then(NpmRepository::url),
            None
        );
        assert!(repository_of(serde_json::json!([])).is_none());
    }

    #[test]
    fn encode_scoped_package() {
        assert_eq!(encode_npm_name("@scope/pkg"), "@scope%2Fpkg");
        assert_eq!(encode_npm_name("lodash"), "lodash");
    }

    #[test]
    fn resolve_dist_tag_known() {
        let mut tags = HashMap::new();
        tags.insert("latest".to_string(), "1.5.0".to_string());
        assert_eq!(resolve_dist_tag(&tags, "latest"), "1.5.0");
    }

    #[test]
    fn resolve_dist_tag_unknown_passes_through() {
        let tags = HashMap::new();
        assert_eq!(resolve_dist_tag(&tags, "2.0.0"), "2.0.0");
    }

    #[test]
    fn pick_checksum_prefers_integrity() {
        let dist = NpmDist {
            tarball: "https://example.com/pkg.tgz".into(),
            integrity: "sha512-abc".into(),
            shasum: "oldsha".into(),
        };
        assert_eq!(pick_checksum(&dist).as_deref(), Some("sha512-abc"));
    }

    #[test]
    fn pick_checksum_falls_back_to_shasum() {
        let dist = NpmDist {
            tarball: "https://example.com/pkg.tgz".into(),
            integrity: String::new(),
            shasum: "abc123".into(),
        };
        assert_eq!(pick_checksum(&dist).as_deref(), Some("abc123"));
    }

    #[test]
    fn pick_checksum_none_when_both_empty() {
        let dist = NpmDist {
            tarball: "https://example.com/pkg.tgz".into(),
            integrity: String::new(),
            shasum: String::new(),
        };
        assert!(pick_checksum(&dist).is_none());
    }

    #[tokio::test]
    async fn fetch_artifact_rejects_cross_origin_tarball_url() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "dist-tags": {"latest": "1.0.0"},
            "versions": {
                "1.0.0": {
                    "version": "1.0.0",
                    "dist": {"tarball": "https://evil.example.com/lodash-1.0.0.tgz"}
                }
            }
        }"#;
        let _mock = server
            .mock("GET", "/lodash")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = NpmRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&PackageId::new("npm", "lodash", "1.0.0").with_artifact("tarball"))
            .await;

        assert!(matches!(result, Err(CoreError::Registry(_))));
    }
    // ── README capture (RFC 0007 §2.1) ────────────────────────────────────────

    /// A packument that carries the text per version answers with that
    /// version's own README, not the package's.
    #[tokio::test]
    async fn a_per_version_readme_reaches_the_extra_channel() {
        let mut server = mockito::Server::new_async().await;
        let body = r##"{
            "dist-tags": {"latest": "2.0.0"},
            "readme": "# the package README",
            "versions": {
                "1.0.0": {
                    "version": "1.0.0",
                    "readme": "# the 1.x README",
                    "dist": {"tarball": "URL/lodash-1.0.0.tgz"}
                },
                "2.0.0": {
                    "version": "2.0.0",
                    "readme": "# the 2.x README",
                    "dist": {"tarball": "URL/lodash-2.0.0.tgz"}
                }
            }
        }"##
        .replace("URL", &server.url());
        let _mock = server
            .mock("GET", "/lodash")
            .with_status(200)
            .with_body(body)
            .expect(2)
            .create_async()
            .await;

        let client = NpmRegistryClient::new(server.url(), &Default::default()).unwrap();

        for (version, expected) in [("1.0.0", "# the 1.x README"), ("2.0.0", "# the 2.x README")] {
            let meta = client
                .resolve_metadata(&PackageId::new("npm", "lodash", version))
                .await
                .unwrap();
            let found = MetadataReadme::from_extra(&meta.extra).expect("readme captured");
            assert_eq!(found.content.as_deref(), Some(expected));
            assert_eq!(found.format, ReadmeFormat::Markdown);
            // A version's own README is its own, not a package-level guess —
            // even for the version `dist-tags.latest` names.
            assert!(!found.package_level);
        }
    }

    /// The document-root README is attributed to the version `dist-tags.latest`
    /// names, labelled as package-level, and **not** to any other version:
    /// inventing a per-version claim from a package-level field would show
    /// 2.x's API to a 1.x reader (RFC 0007 §2.4).
    #[tokio::test]
    async fn the_root_readme_is_attributed_only_to_latest_and_labelled() {
        let mut server = mockito::Server::new_async().await;
        let body = r##"{
            "dist-tags": {"latest": "2.0.0"},
            "readme": "# the package README",
            "versions": {
                "1.0.0": {"version": "1.0.0", "dist": {"tarball": "URL/a.tgz"}},
                "2.0.0": {"version": "2.0.0", "dist": {"tarball": "URL/b.tgz"}}
            }
        }"##
        .replace("URL", &server.url());
        let _mock = server
            .mock("GET", "/lodash")
            .with_status(200)
            .with_body(body)
            .expect(2)
            .create_async()
            .await;

        let client = NpmRegistryClient::new(server.url(), &Default::default()).unwrap();

        let latest = client
            .resolve_metadata(&PackageId::new("npm", "lodash", "2.0.0"))
            .await
            .unwrap();
        let found = MetadataReadme::from_extra(&latest.extra).expect("readme captured");
        assert_eq!(found.content.as_deref(), Some("# the package README"));
        assert!(found.package_level);

        let older = client
            .resolve_metadata(&PackageId::new("npm", "lodash", "1.0.0"))
            .await
            .unwrap();
        assert_eq!(MetadataReadme::from_extra(&older.extra), None);
    }

    /// npm writes a *string* when the tarball had no README, so a naïve
    /// `Option::is_some` would store an error message as documentation.
    #[tokio::test]
    async fn npms_missing_readme_placeholder_is_not_a_readme() {
        let mut server = mockito::Server::new_async().await;
        let body = r##"{
            "dist-tags": {"latest": "1.0.0"},
            "readme": "ERROR: No README data found!",
            "versions": {
                "1.0.0": {
                    "version": "1.0.0",
                    "readme": "   ",
                    "dist": {"tarball": "URL/a.tgz"}
                }
            }
        }"##
        .replace("URL", &server.url());
        let _mock = server
            .mock("GET", "/lodash")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let client = NpmRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&PackageId::new("npm", "lodash", "1.0.0"))
            .await
            .unwrap();
        assert_eq!(MetadataReadme::from_extra(&meta.extra), None);
    }

    /// A packument with no README anywhere costs nothing and claims nothing —
    /// npm's tarball may still have one, which is why its support is
    /// `MetadataThenArchive`.
    #[tokio::test]
    async fn a_packument_without_a_readme_captures_nothing() {
        let mut server = mockito::Server::new_async().await;
        let body = r##"{
            "dist-tags": {"latest": "1.0.0"},
            "versions": {"1.0.0": {"version": "1.0.0", "dist": {"tarball": "URL/a.tgz"}}}
        }"##
        .replace("URL", &server.url());
        let _mock = server
            .mock("GET", "/lodash")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let client = NpmRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&PackageId::new("npm", "lodash", "1.0.0"))
            .await
            .unwrap();
        assert_eq!(MetadataReadme::from_extra(&meta.extra), None);
    }
}
