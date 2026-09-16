use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{FetchedArtifact, RegistryClient, UpstreamPackage},
};

use super::super::http_client::{
    basic_auth_get, cache_control, new_http_client, percent_encode, to_registry_error,
    UpstreamHttpOptions,
};
use super::models::{
    parse_plugin_list, PluginIdentity, PluginListEntry, PluginUpdate, SearchPluginsResponse,
};

/// JetBrains Marketplace client (plugins.jetbrains.com or compatible).
///
/// Supported `PackageId` conventions:
/// - `name`: the plugin xmlId (`<id>` from `META-INF/plugin.xml`), e.g.
///   `"org.rust.lang"`. For the IDE `/files/` passthrough the name/version
///   carry the upstream numeric ids verbatim instead.
/// - `version = "latest"` → newest version on the default (Stable) channel
/// - `artifact = None` / `Some("plugin")` → `/plugin/download` archive
/// - `artifact = Some("plugin@{channel}")` → same, from a release channel
/// - `artifact = Some("file/{fileName}")` → `/files/{name}/{version}/{fileName}`
pub struct JetbrainsMarketplaceRegistryClient {
    http: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
}

impl JetbrainsMarketplaceRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Ok(Self {
            http,
            base_url,
            basic_auth: opts.basic_auth.clone(),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// Fetch every published version of `xml_id` in one hop.
    ///
    /// The marketplace answers an *empty* `<plugin-repository/>` (HTTP 200) for
    /// unknown ids, so emptiness maps to `NotFound` alongside a plain 404.
    async fn fetch_plugin_list(&self, xml_id: &str) -> Result<Vec<PluginListEntry>, CoreError> {
        let url = format!(
            "{}/plugins/list?pluginId={}",
            self.base_url,
            percent_encode(xml_id)
        );
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "JetBrains plugin '{xml_id}' not found"
            )));
        }
        let body = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .text()
            .await
            .map_err(to_registry_error)?;
        let entries = parse_plugin_list(body.as_bytes())?;
        if entries.is_empty() {
            return Err(CoreError::NotFound(format!(
                "JetBrains plugin '{xml_id}' not found"
            )));
        }
        Ok(entries)
    }

    /// Whether a path segment is one of the marketplace's numeric ids.
    ///
    /// A plugin xmlId is a reverse-DNS-ish string (`org.rust.lang`, `IdeaVIM`)
    /// and a version is dotted; neither is all digits, so "all digits" is an
    /// unambiguous signal that this is the `/files/{pluginId}/{updateId}/…`
    /// spelling rather than the published one.
    fn is_numeric_id(segment: &str) -> bool {
        !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit())
    }

    /// `GET /api/plugins/{pluginId}/updates` — every update of one plugin, with
    /// the numeric id the IDE addresses it by beside the version string this
    /// server publishes.
    async fn fetch_updates(&self, plugin_id: &str) -> Result<Vec<PluginUpdate>, CoreError> {
        let url = format!(
            "{}/api/plugins/{}/updates",
            self.base_url,
            percent_encode(plugin_id)
        );
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "JetBrains plugin '{plugin_id}' not found"
            )));
        }
        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<Vec<PluginUpdate>>()
            .await
            .map_err(to_registry_error)
    }

    /// `GET /api/plugins/{pluginId}` — the plugin's `xmlId`, which is the only
    /// part of the canonical coordinate the updates listing does not carry (its
    /// `link` holds a URL slug, `164-ideavim`, which is not the id).
    async fn fetch_xml_id(&self, plugin_id: &str) -> Result<String, CoreError> {
        let url = format!(
            "{}/api/plugins/{}",
            self.base_url,
            percent_encode(plugin_id)
        );
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "JetBrains plugin '{plugin_id}' not found"
            )));
        }
        let plugin = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json::<PluginIdentity>()
            .await
            .map_err(to_registry_error)?;
        plugin.xml_id.ok_or_else(|| {
            CoreError::Registry(format!("JetBrains plugin '{plugin_id}' carries no xmlId"))
        })
    }

    /// Newest entry by publish date; falls back to the first listed entry when
    /// no entry carries a date (the upstream lists newest first).
    fn latest_entry(entries: &[PluginListEntry]) -> &PluginListEntry {
        entries
            .iter()
            .max_by_key(|e| e.date_ms.unwrap_or(i64::MIN))
            .unwrap_or(&entries[0])
    }
}

#[async_trait]
impl RegistryClient for JetbrainsMarketplaceRegistryClient {
    fn registry_type(&self) -> &str {
        "jetbrains-marketplace"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let entries = self.fetch_plugin_list(&pkg.name).await?;

        let resolved = if pkg.version == "latest" {
            Self::latest_entry(&entries)
        } else {
            entries
                .iter()
                .find(|e| e.version.as_deref() == Some(pkg.version.as_str()))
                .ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "JetBrains plugin {}@{} not found",
                        pkg.name, pkg.version
                    ))
                })?
        };

        let resolved_version = resolved
            .version
            .clone()
            .unwrap_or_else(|| pkg.version.clone());
        let published_at = resolved
            .date_ms
            .and_then(chrono::DateTime::from_timestamp_millis);

        // The full version list rides along in `extra` so the web layer can
        // render every per-plugin endpoint shape (plugin-list XML, plugin JSON,
        // updates array, meta.json) from one cached metadata entry — the
        // offline-resilience backbone.
        let versions: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "version": e.version,
                    "since_build": e.since_build,
                    "until_build": e.until_build,
                    // `/plugins/list` is queried without a channel param and
                    // its XML carries no channel field: every entry here is
                    // from the default (Stable) channel by construction.
                    // Channel-specific *downloads* still work via the
                    // `plugin@{channel}` artifact suffix in `fetch_artifact`.
                    "channel": "",
                    "date_ms": e.date_ms,
                    "size": e.size,
                })
            })
            .collect();

        let extra = serde_json::json!({
            "resolved_version": resolved_version,
            "name": resolved.name,
            "vendor": resolved.vendor,
            "description": resolved.description,
            "change_notes": resolved.change_notes,
            "depends": resolved.depends,
            "since_build": resolved.since_build,
            "until_build": resolved.until_build,
            "versions": versions,
        });

        Ok(PackageMetadata {
            id: PackageId {
                version: resolved_version,
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

    /// Versions **oldest-first**, per the `RegistryClient::list_versions`
    /// contract — `/plugins/list` answers newest-first, so the upstream order is
    /// inverted and then sorted by publish date (a stable sort, so entries
    /// without a `date` attribute keep the inverted upstream order and stay
    /// ahead of the dated ones).
    ///
    /// Ordering is not cosmetic here: warming takes the *last* `warm_latest_n`
    /// entries, so a newest-first list would pre-fetch the oldest releases.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let entries = self.fetch_plugin_list(package).await?;
        let mut ordered: Vec<&PluginListEntry> = entries.iter().rev().collect();
        ordered.sort_by_key(|e| e.date_ms.unwrap_or(i64::MIN));

        let mut versions: Vec<String> = Vec::new();
        for e in ordered {
            if let Some(v) = &e.version {
                if !versions.contains(v) {
                    versions.push(v.clone());
                }
            }
        }
        Ok(versions)
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let url = match pkg.artifact.as_deref() {
            None | Some("plugin") => format!(
                "{}/plugin/download?pluginId={}&version={}",
                self.base_url,
                percent_encode(&pkg.name),
                percent_encode(&pkg.version),
            ),
            Some(a) if a.starts_with("plugin@") => {
                let channel = &a["plugin@".len()..];
                format!(
                    "{}/plugin/download?pluginId={}&version={}&channel={}",
                    self.base_url,
                    percent_encode(&pkg.name),
                    percent_encode(&pkg.version),
                    percent_encode(channel),
                )
            }
            // IDE /files/ passthrough: name/version carry the upstream numeric
            // ids verbatim.
            Some(a) if a.starts_with("file/") => {
                let file_name = &a["file/".len()..];
                format!(
                    "{}/files/{}/{}/{}",
                    self.base_url,
                    percent_encode(&pkg.name),
                    percent_encode(&pkg.version),
                    percent_encode(file_name),
                )
            }
            Some(other) => {
                return Err(CoreError::Registry(format!(
                    "unsupported JetBrains Marketplace artifact '{other}'"
                )));
            }
        };

        tracing::debug!(url = %url, "fetching JetBrains Marketplace artifact");

        // The download endpoint 302-redirects to the CDN; the shared client
        // follows redirects, and the URL is built from our own configured base
        // (not attacker-supplied), so no same-origin pin applies here.
        let response = self.get(&url).send().await.map_err(to_registry_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "JetBrains plugin artifact {}@{} not found",
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

    /// `files/{pluginId}/{updateId}/…` → `xmlId@version`.
    ///
    /// The IDE reads the numeric pair out of `api/search/updates/compatible`
    /// (`{"id":1149038,"pluginId":164,…}`) and addresses the update by it. Two
    /// upstream calls, both cached by `ProxyService::canonical_coordinate`, and
    /// the artifact selector rides along unchanged — once the coordinate is
    /// canonical the archive is fetched by `plugin/download` like any other.
    async fn canonical_coordinate(&self, pkg: &PackageId) -> Result<Option<PackageId>, CoreError> {
        if !(Self::is_numeric_id(&pkg.name) && Self::is_numeric_id(&pkg.version)) {
            return Ok(None);
        }

        let updates = self.fetch_updates(&pkg.name).await?;
        let version = updates
            .iter()
            .find(|u| u.id.to_string() == pkg.version)
            .and_then(|u| u.version.clone())
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "JetBrains update {} of plugin {} not found",
                    pkg.version, pkg.name
                ))
            })?;
        let xml_id = self.fetch_xml_id(&pkg.name).await?;

        Ok(Some(PackageId {
            name: xml_id,
            version,
            ..pkg.clone()
        }))
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        let url = format!(
            "{}/api/searchPlugins?search={}&max={}",
            self.base_url,
            percent_encode(query),
            limit.min(50),
        );

        let res = self.get(&url).send().await.map_err(to_registry_error)?;
        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: SearchPluginsResponse = res.json().await.map_err(to_registry_error)?;
        Ok(body
            .plugins
            .into_iter()
            .map(|p| UpstreamPackage {
                name: p.xml_id,
                // Search hits carry no version; the proxy convention resolves
                // "latest" through `resolve_metadata`.
                latest_version: "latest".to_owned(),
                description: p.preview,
            })
            .collect())
    }
}
