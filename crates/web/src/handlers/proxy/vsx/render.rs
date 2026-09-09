//! Turning entries into the two protocols' documents. Pure — no actix, no
//! services, no I/O — so every shape here is unit-testable.
//!
//! One intermediate, [`GalleryEntry`], is produced by both the local and the
//! proxy path, and every document is a function of it. That is the same
//! arrangement `jetbrains_marketplace/render.rs` uses, and it is what lets
//! blocked-version filtering happen once, upstream of all of them.

use serde_json::{json, Value};

use super::protocol::{asset_type, flag, property_key, GalleryQuery};
use batlehub_core::services::OpenVsxExtensionVersion;

/// One extension, with the versions that survived filtering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GalleryEntry {
    pub publisher: String,
    pub extension_name: String,
    /// The stable identity the editor keys installs on. See
    /// [`synthetic_extension_uuid`].
    pub extension_id: String,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub install_count: u64,
    pub versions: Vec<GalleryVersion>,
    /// The upstream extension object this was parsed from, when there was one.
    ///
    /// Rendering starts from this and overwrites only the fields the proxy owns
    /// (`versions`, and the URLs inside them). Everything the marketplace sends
    /// that this proxy has never modelled — properties, statistics, flags that
    /// vary by marketplace version — therefore survives the round trip. A
    /// parse-and-rebuild would silently drop them, and the loss would only show
    /// up as a subtly wrong extension pane.
    pub upstream: Option<Value>,
}

/// Where a version's signature asset comes from (RFC 0020 §5.1). `None` is
/// a version nobody signed, and it advertises no signature asset: an
/// advertised asset a request cannot fetch is the failure the editor reports
/// worst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureSource {
    /// Signed by this registry's key; the public key is this registry's.
    Registry { key_id: String },
    /// Relayed from the upstream that signed it; `public_key` when that
    /// upstream names one (Open VSX does, the Microsoft marketplace does not).
    Upstream { public_key: bool },
    /// Provided at publish time (RFC 0020 §13.6): an upstream's archive
    /// attached to a locally held version, served as-is. Its key is its
    /// signer's, so no `PublicKey` asset is advertised.
    Provided,
}

impl SignatureSource {
    fn has_public_key(&self) -> bool {
        match self {
            Self::Registry { .. } => true,
            Self::Upstream { public_key } => *public_key,
            Self::Provided => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GalleryVersion {
    pub version: String,
    /// The signature asset this version carries, if any.
    pub signature: Option<SignatureSource>,
    /// RFC 3339.
    pub last_updated: String,
    /// `engines.vscode`. Absent is **not** the same as permissive: the editor
    /// falls back to reading the manifest asset, which this server serves.
    pub engine: Option<String>,
    pub extension_pack: Vec<String>,
    pub extension_dependencies: Vec<String>,
    pub pre_release: bool,
}

/// The extension-level `flags` string, as the marketplace reports it for an
/// ordinary published extension. See where it is inserted in [`extension_json`]
/// for why it is never omitted.
const EXTENSION_FLAGS: &str = "validated public";

/// `{publisher}.{name}`.
pub fn qualified_name(publisher: &str, name: &str) -> String {
    format!("{publisher}.{name}")
}

/// A deterministic UUID for an extension that has no upstream one.
///
/// VS Code stores this as the extension's identity and uses it to detect
/// renames and to deduplicate installs, so it has to be **stable across
/// restarts and across every BatleHub instance** — a random one would make the
/// editor believe a locally published extension changed identity on every
/// server bounce.
///
/// UUIDv5 over the qualified name gives exactly that, with no state to keep.
pub fn synthetic_extension_uuid(publisher: &str, name: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        qualified_name(publisher, name).as_bytes(),
    )
    .to_string()
}

impl GalleryEntry {
    /// Build from the local registry's typed versions, newest first.
    ///
    /// `versions` must be non-empty; callers get them from
    /// `get_openvsx_versions`, which errors rather than returning an empty
    /// list.
    pub fn from_local(versions: &[OpenVsxExtensionVersion]) -> Option<Self> {
        let newest = versions.iter().max_by_key(|v| v.published_at)?;
        let mut ordered: Vec<&OpenVsxExtensionVersion> = versions.iter().collect();
        ordered.sort_by_key(|v| std::cmp::Reverse(v.published_at));

        Some(Self {
            publisher: newest.publisher.clone(),
            extension_name: newest.name.clone(),
            extension_id: synthetic_extension_uuid(&newest.publisher, &newest.name),
            display_name: newest.display_name.clone(),
            short_description: newest.description.clone(),
            categories: newest.categories.clone(),
            tags: newest.tags.clone(),
            // A private registry keeps no install counter. Reported as zero
            // rather than omitted, because the editor sorts by it and a missing
            // key reads as malformed.
            install_count: 0,
            versions: ordered
                .into_iter()
                .map(|v| GalleryVersion {
                    version: v.version.clone(),
                    last_updated: v.published_at.to_rfc3339(),
                    engine: v.engine.clone(),
                    extension_pack: v.extension_pack.clone(),
                    extension_dependencies: v.dependencies.clone(),
                    pre_release: v.pre_release,
                    signature: v.signature_provided.then_some(SignatureSource::Provided),
                })
                .collect(),
            upstream: None,
        })
    }

    /// Parse one `extensionquery` extension object from an upstream response.
    pub fn from_upstream(obj: &Value) -> Option<Self> {
        let publisher = obj
            .get("publisher")
            .and_then(|p| p.get("publisherName"))
            .and_then(Value::as_str)?
            .to_owned();
        let extension_name = obj.get("extensionName").and_then(Value::as_str)?.to_owned();

        let versions = obj
            .get("versions")
            .and_then(Value::as_array)
            .map(|vs| {
                vs.iter()
                    .filter_map(GalleryVersion::from_upstream)
                    .collect()
            })
            .unwrap_or_default();

        Some(Self {
            extension_id: obj
                .get("extensionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| synthetic_extension_uuid(&publisher, &extension_name)),
            display_name: obj
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            short_description: obj
                .get("shortDescription")
                .and_then(Value::as_str)
                .map(str::to_owned),
            categories: string_list(obj.get("categories")),
            tags: string_list(obj.get("tags")),
            install_count: obj
                .get("statistics")
                .and_then(Value::as_array)
                .and_then(|stats| {
                    stats
                        .iter()
                        .find(|s| s.get("statisticName").and_then(Value::as_str) == Some("install"))
                        .and_then(|s| s.get("value"))
                        .and_then(Value::as_f64)
                })
                .unwrap_or(0.0) as u64,
            publisher,
            extension_name,
            versions,
            upstream: Some(obj.clone()),
        })
    }

    /// Every version is signed by this registry's key (RFC 0020 §4.2): the
    /// local branch of `source.rs`, once the registry is known to hold one.
    ///
    /// A version whose archive was provided keeps it (§13.6): the registry
    /// signs nothing that someone else signed.
    pub fn sign_locally(&mut self, key_id: &str) {
        for v in &mut self.versions {
            if v.signature == Some(SignatureSource::Provided) {
                continue;
            }
            v.signature = Some(SignatureSource::Registry {
                key_id: key_id.to_owned(),
            });
        }
    }

    pub fn qualified_name(&self) -> String {
        qualified_name(&self.publisher, &self.extension_name)
    }

    /// Whether `query`'s free-text and facet criteria select this entry.
    ///
    /// Substring, case-insensitive, over the fields a user would type.
    /// Deliberately not ranked: a private registry holds tens of extensions,
    /// and a relevance model nobody can explain is worse than an obvious one.
    pub fn matches(&self, query: &GalleryQuery) -> bool {
        if !query.extension_names.is_empty()
            && !query
                .extension_names
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&self.qualified_name()))
        {
            return false;
        }
        if !query.extension_ids.is_empty()
            && !query
                .extension_ids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&self.extension_id))
        {
            return false;
        }
        if !query
            .tags
            .iter()
            .all(|t| self.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
        {
            return false;
        }
        if !query
            .categories
            .iter()
            .all(|c| self.categories.iter().any(|x| x.eq_ignore_ascii_case(c)))
        {
            return false;
        }
        if let Some(text) = query.search_text.as_deref() {
            let q = text.to_lowercase();
            let hit = |s: &str| s.to_lowercase().contains(&q);
            let found = hit(&self.qualified_name())
                || self.display_name.as_deref().is_some_and(hit)
                || self.short_description.as_deref().is_some_and(hit)
                || self.tags.iter().any(|t| hit(t))
                || self.categories.iter().any(|c| hit(c));
            if !found {
                return false;
            }
        }
        true
    }
}

impl GalleryVersion {
    fn from_upstream(v: &Value) -> Option<Self> {
        let props = v.get("properties").and_then(Value::as_array);
        let prop = |key: &str| -> Option<String> {
            props?
                .iter()
                .find(|p| p.get("key").and_then(Value::as_str) == Some(key))
                .and_then(|p| p.get("value"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let csv = |key: &str| -> Vec<String> {
            prop(key)
                .map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|x| !x.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };

        Some(Self {
            version: v.get("version").and_then(Value::as_str)?.to_owned(),
            last_updated: v
                .get("lastUpdated")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            engine: prop(property_key::ENGINE),
            extension_pack: csv(property_key::EXTENSION_PACK),
            extension_dependencies: csv(property_key::EXTENSION_DEPENDENCIES),
            pre_release: prop(property_key::PRE_RELEASE).as_deref() == Some("true"),
            signature: None,
        })
    }

    /// The asset types this version advertises: the six every version has,
    /// plus the signature and its key when the version carries them.
    pub fn asset_types(&self) -> Vec<&'static str> {
        let mut types: Vec<&'static str> = asset_type::ALL.to_vec();
        if let Some(sig) = &self.signature {
            types.push(asset_type::SIGNATURE);
            if sig.has_public_key() {
                types.push(asset_type::PUBLIC_KEY);
            }
        }
        types
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// ── Documents ────────────────────────────────────────────────────────────────

/// Where an editor should fetch this registry's assets from.
///
/// Built from `registry_public_base`, never from a literal path — that is the
/// one function that gets host routing and the forwarded-header trust boundary
/// right.
#[derive(Debug, Clone)]
pub struct GalleryUrls {
    base: String,
}

impl GalleryUrls {
    pub fn new(public_base: &str) -> Self {
        Self {
            base: public_base.trim_end_matches('/').to_owned(),
        }
    }

    /// `assetUri` — the editor appends `/{assetType}` to this.
    pub fn asset_base(&self, publisher: &str, name: &str, version: &str) -> String {
        format!("{}/vscode/asset/{publisher}/{name}/{version}", self.base)
    }

    pub fn asset(&self, publisher: &str, name: &str, version: &str, asset_type: &str) -> String {
        format!("{}/{asset_type}", self.asset_base(publisher, name, version))
    }
}

/// One extension object for an `extensionquery` response.
///
/// Starts from `entry.upstream` when there is one and overwrites the fields the
/// proxy owns, so unmodelled upstream keys survive. The overwritten set is
/// closed and small: `versions` (filtered, and re-pointed at this host),
/// `publisher`, `extensionName`, `extensionId`, and the flag-gated blocks.
pub fn extension_json(entry: &GalleryEntry, urls: &GalleryUrls, query: &GalleryQuery) -> Value {
    let mut out = match &entry.upstream {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => json!({}),
    };
    let obj = out.as_object_mut().expect("object");

    obj.insert(
        "publisher".to_owned(),
        json!({
            "publisherName": entry.publisher,
            "displayName": entry.publisher,
        }),
    );
    obj.insert("extensionId".to_owned(), json!(entry.extension_id));
    obj.insert("extensionName".to_owned(), json!(entry.extension_name));
    obj.insert(
        "displayName".to_owned(),
        json!(entry
            .display_name
            .clone()
            .unwrap_or_else(|| entry.extension_name.clone())),
    );
    obj.insert(
        "shortDescription".to_owned(),
        json!(entry.short_description.clone().unwrap_or_default()),
    );

    // `flags` is a space-separated **string**, and the editor reads it with no
    // guard at all: `preview: flags.indexOf("preview") !== -1`. Omitted, that is
    // `Cannot read properties of undefined (reading 'indexOf')` and the install
    // dies before a single asset is fetched.
    //
    // `preview` is never among the flags this server reports: the manifest's
    // `preview` field is not carried through the local registry, so claiming it
    // either way would be a guess. The cost is a missing badge in the extension
    // pane. An upstream string is kept as it came, since the marketplace does
    // know.
    if !obj.get("flags").is_some_and(Value::is_string) {
        obj.insert("flags".to_owned(), json!(EXTENSION_FLAGS));
    }

    // `Date.parse`d unguarded, so an absent value is `NaN` rather than a crash —
    // but then every date the extension pane shows is blank. Both are derived
    // from the versions, newest first, and an upstream value wins because it
    // knows the history this proxy may only hold part of.
    if !obj.get("lastUpdated").is_some_and(Value::is_string) {
        if let Some(newest) = entry.versions.first() {
            obj.insert("lastUpdated".to_owned(), json!(newest.last_updated));
        }
    }
    if !obj.get("releaseDate").is_some_and(Value::is_string) {
        if let Some(oldest) = entry.versions.last() {
            obj.insert("releaseDate".to_owned(), json!(oldest.last_updated));
        }
    }

    if query.wants(flag::INCLUDE_CATEGORY_AND_TAGS) {
        obj.insert("categories".to_owned(), json!(entry.categories));
        obj.insert("tags".to_owned(), json!(entry.tags));
    } else {
        obj.insert("categories".to_owned(), json!([]));
        obj.insert("tags".to_owned(), json!([]));
    }

    if query.wants(flag::INCLUDE_STATISTICS) {
        obj.insert(
            "statistics".to_owned(),
            json!([
                { "statisticName": "install", "value": entry.install_count },
                { "statisticName": "averagerating", "value": 0 },
                { "statisticName": "ratingcount", "value": 0 },
            ]),
        );
    }

    // The flags choose **how many** versions ship, never whether any do.
    //
    // VS Code does not send `IncludeVersions` (0x1) for an install or an update
    // check — it sends `IncludeLatestVersionOnly` (0x200) and expects the latest
    // version in `versions` anyway, which is what the marketplace answers with.
    // Its gallery service then reads `versions[0].files` without checking, so an
    // empty array is not "versions were not requested", it is
    // `Cannot read properties of undefined (reading 'files')` and a failed
    // install. (The editor also drops `IncludeVersions` itself whenever both
    // flags are set, which is why the two are read as one choice here.)
    //
    // Truncating happens **after** blocked versions were removed upstream of
    // here. Truncating first would offer the editor a blocked build as *the*
    // version, and the install would then be refused — the exact failure hiding
    // exists to prevent.
    let versions: Vec<&GalleryVersion> =
        if query.wants(flag::INCLUDE_VERSIONS) && !query.wants(flag::INCLUDE_LATEST_VERSION_ONLY) {
            entry.versions.iter().collect()
        } else {
            entry.versions.iter().take(1).collect()
        };

    obj.insert(
        "versions".to_owned(),
        json!(versions
            .iter()
            .map(|v| version_json(entry, v, urls, query))
            .collect::<Vec<_>>()),
    );

    out
}

fn version_json(
    entry: &GalleryEntry,
    v: &GalleryVersion,
    urls: &GalleryUrls,
    query: &GalleryQuery,
) -> Value {
    let asset_base = urls.asset_base(&entry.publisher, &entry.extension_name, &v.version);

    let mut out = json!({
        "version": v.version,
        "lastUpdated": v.last_updated,
        // Emitted regardless of `IncludeAssetUri`. A client that did not ask
        // ignores two strings; a client that needed them and forgot the flag
        // would otherwise have no way to reach any asset at all, and a
        // hand-written product.json gets flags wrong far more often than it
        // gets URLs wrong.
        "assetUri": asset_base,
        "fallbackAssetUri": asset_base,
    });
    let obj = out.as_object_mut().expect("object");

    if query.wants(flag::INCLUDE_FILES) {
        obj.insert(
            "files".to_owned(),
            json!(v
                .asset_types()
                .into_iter()
                .map(|t| json!({
                    "assetType": t,
                    "source": urls.asset(&entry.publisher, &entry.extension_name, &v.version, t),
                }))
                .collect::<Vec<_>>()),
        );
    } else {
        obj.insert("files".to_owned(), json!([]));
    }

    if query.wants(flag::INCLUDE_VERSION_PROPERTIES) {
        let mut props = Vec::new();
        if let Some(engine) = &v.engine {
            props.push(json!({ "key": property_key::ENGINE, "value": engine }));
        }
        if !v.extension_pack.is_empty() {
            props.push(json!({
                "key": property_key::EXTENSION_PACK,
                "value": v.extension_pack.join(","),
            }));
        }
        if !v.extension_dependencies.is_empty() {
            props.push(json!({
                "key": property_key::EXTENSION_DEPENDENCIES,
                "value": v.extension_dependencies.join(","),
            }));
        }
        if v.pre_release {
            props.push(json!({ "key": property_key::PRE_RELEASE, "value": "true" }));
        }
        obj.insert("properties".to_owned(), json!(props));
    }

    out
}

/// The `extensionquery` response envelope.
///
/// `total` is the count **before** paging and **after** filtering — the editor
/// uses it to decide whether to ask for another page, so a count that ignores
/// either stops "load more" a page early or spins forever on an empty one.
pub fn query_response_json(
    entries: &[GalleryEntry],
    total: usize,
    urls: &GalleryUrls,
    query: &GalleryQuery,
) -> Value {
    // An entry with no version is dropped rather than rendered: the editor
    // dereferences `versions[0]` unconditionally, so listing an extension it
    // cannot resolve a version for crashes its install rather than telling it
    // there is nothing to install. Every caller already filters these out
    // (`source::extension_entry` returns `None`, `from_local` needs a version),
    // so this states the invariant rather than serving a use case.
    let extensions: Vec<Value> = entries
        .iter()
        .filter(|e| {
            if e.versions.is_empty() {
                tracing::warn!(
                    extension = %e.qualified_name(),
                    "dropped a gallery entry with no visible version"
                );
            }
            !e.versions.is_empty()
        })
        .map(|e| extension_json(e, urls, query))
        .collect();

    json!({
        "results": [{
            "extensions": extensions,
            "pagingToken": Value::Null,
            "resultMetadata": [{
                "metadataType": "ResultCount",
                "metadataItems": [{ "name": "TotalCount", "count": total }],
            }],
        }]
    })
}

/// One extension for the OpenVSX native API (`/api/{namespace}/{extension}`).
pub fn openvsx_extension_json(
    entry: &GalleryEntry,
    version: &GalleryVersion,
    all_versions: &[GalleryVersion],
    urls: &GalleryUrls,
) -> Value {
    let file = |t: &str| urls.asset(&entry.publisher, &entry.extension_name, &version.version, t);
    let mut files = serde_json::Map::new();
    files.insert("download".to_owned(), json!(file(asset_type::VSIX_PACKAGE)));
    files.insert("manifest".to_owned(), json!(file(asset_type::MANIFEST)));
    files.insert("readme".to_owned(), json!(file(asset_type::DETAILS)));
    files.insert("changelog".to_owned(), json!(file(asset_type::CHANGELOG)));
    files.insert("license".to_owned(), json!(file(asset_type::LICENSE)));
    files.insert("icon".to_owned(), json!(file(asset_type::ICON)));
    // Open VSX's own two keys for them (RFC 0020 §4.2), under Open VSX's
    // rule: present only when the version is signed.
    if let Some(sig) = &version.signature {
        files.insert("signature".to_owned(), json!(file(asset_type::SIGNATURE)));
        if sig.has_public_key() {
            files.insert("publicKey".to_owned(), json!(file(asset_type::PUBLIC_KEY)));
        }
    }
    json!({
        "namespace": entry.publisher,
        "name": entry.extension_name,
        "version": version.version,
        "timestamp": version.last_updated,
        "displayName": entry.display_name,
        "description": entry.short_description,
        "categories": entry.categories,
        "tags": entry.tags,
        "engines": { "vscode": version.engine },
        "downloadCount": entry.install_count,
        // OpenVSX reports this per version, and `ovsx`/the web UI read it to
        // label a pre-release. Same bit the gallery sends as the
        // `Microsoft.VisualStudio.Code.PreRelease` property.
        "preRelease": version.pre_release,
        "files": files,
        "allVersions": all_versions
            .iter()
            .map(|v| {
                (
                    v.version.clone(),
                    Value::String(format!(
                        "{}/api/{}/{}/{}",
                        urls.base, entry.publisher, entry.extension_name, v.version
                    )),
                )
            })
            .collect::<serde_json::Map<_, _>>(),
    })
}

/// The OpenVSX `/api/-/search` envelope.
pub fn openvsx_search_json(entries: &[GalleryEntry], total: usize, urls: &GalleryUrls) -> Value {
    json!({
        "offset": 0,
        "totalSize": total,
        "extensions": entries
            .iter()
            .filter_map(|e| e.versions.first().map(|v| (e, v)))
            .map(|(e, v)| json!({
                "url": format!("{}/api/{}/{}", urls.base, e.publisher, e.extension_name),
                "files": {
                    "download": urls.asset(&e.publisher, &e.extension_name, &v.version, asset_type::VSIX_PACKAGE),
                    "icon": urls.asset(&e.publisher, &e.extension_name, &v.version, asset_type::ICON),
                },
                "name": e.extension_name,
                "namespace": e.publisher,
                "version": v.version,
                "timestamp": v.last_updated,
                "displayName": e.display_name,
                "description": e.short_description,
                "downloadCount": e.install_count,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::proxy::vsx::protocol::ExtensionQueryRequest;

    const ALL_FLAGS: u32 = flag::INCLUDE_VERSIONS
        | flag::INCLUDE_FILES
        | flag::INCLUDE_CATEGORY_AND_TAGS
        | flag::INCLUDE_VERSION_PROPERTIES
        | flag::INCLUDE_ASSET_URI
        | flag::INCLUDE_STATISTICS;

    fn urls() -> GalleryUrls {
        GalleryUrls::new("https://hub.example.com/proxy/vsx1/")
    }

    fn q(flags: u32) -> GalleryQuery {
        GalleryQuery::from_request(&ExtensionQueryRequest {
            flags,
            ..Default::default()
        })
    }

    fn entry() -> GalleryEntry {
        GalleryEntry {
            publisher: "acme".to_owned(),
            extension_name: "tool".to_owned(),
            extension_id: synthetic_extension_uuid("acme", "tool"),
            display_name: Some("Acme Tool".to_owned()),
            short_description: Some("Does the thing".to_owned()),
            categories: vec!["Linters".to_owned()],
            tags: vec!["acme".to_owned()],
            install_count: 0,
            versions: vec![
                GalleryVersion {
                    version: "2.0.0".to_owned(),
                    last_updated: "2024-03-01T00:00:00+00:00".to_owned(),
                    engine: Some("^1.85.0".to_owned()),
                    extension_pack: vec!["acme.other".to_owned()],
                    extension_dependencies: vec![],
                    pre_release: false,
                    signature: None,
                },
                GalleryVersion {
                    version: "1.0.0".to_owned(),
                    last_updated: "2024-01-01T00:00:00+00:00".to_owned(),
                    engine: Some("^1.80.0".to_owned()),
                    extension_pack: vec![],
                    extension_dependencies: vec![],
                    pre_release: false,
                    signature: None,
                },
            ],
            upstream: None,
        }
    }

    #[test]
    fn the_uuid_is_stable_and_distinct_per_extension() {
        assert_eq!(
            synthetic_extension_uuid("acme", "tool"),
            synthetic_extension_uuid("acme", "tool"),
            "the editor keys installs on this; it cannot change between restarts"
        );
        assert_ne!(
            synthetic_extension_uuid("acme", "tool"),
            synthetic_extension_uuid("acme", "other")
        );
    }

    #[test]
    fn every_generated_url_points_at_this_host() {
        let doc = extension_json(&entry(), &urls(), &q(ALL_FLAGS));
        let v = &doc["versions"][0];

        assert_eq!(
            v["assetUri"], "https://hub.example.com/proxy/vsx1/vscode/asset/acme/tool/2.0.0",
            "the trailing slash on the base must not double up"
        );
        assert_eq!(v["assetUri"], v["fallbackAssetUri"]);

        let vsix = v["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["assetType"] == asset_type::VSIX_PACKAGE)
            .unwrap();
        assert_eq!(
            vsix["source"],
            "https://hub.example.com/proxy/vsx1/vscode/asset/acme/tool/2.0.0/\
             Microsoft.VisualStudio.Services.VSIXPackage"
        );
    }

    /// Asset URIs go out whether or not the flag asked for them — a
    /// hand-written `product.json` gets flags wrong far more often than URLs.
    #[test]
    fn asset_uris_are_emitted_even_without_the_flag() {
        let doc = extension_json(&entry(), &urls(), &q(flag::INCLUDE_VERSIONS));
        assert!(doc["versions"][0]["assetUri"].is_string());
    }

    #[test]
    fn all_six_asset_types_are_advertised() {
        let doc = extension_json(&entry(), &urls(), &q(ALL_FLAGS));
        let types: Vec<&str> = doc["versions"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["assetType"].as_str().unwrap())
            .collect();
        assert_eq!(types, asset_type::ALL);
    }

    #[test]
    fn a_signed_version_advertises_the_signature_and_its_key() {
        let mut e = entry();
        e.sign_locally("abc123");
        let doc = extension_json(&e, &urls(), &q(ALL_FLAGS));
        let types: Vec<&str> = doc["versions"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["assetType"].as_str().unwrap())
            .collect();
        assert!(types.contains(&asset_type::SIGNATURE), "{types:?}");
        assert!(types.contains(&asset_type::PUBLIC_KEY), "{types:?}");
        let ovsx = openvsx_extension_json(&e, &e.versions[0], &e.versions, &urls());
        assert!(ovsx["files"]["signature"]
            .as_str()
            .unwrap()
            .ends_with(asset_type::SIGNATURE));
        assert!(ovsx["files"]["publicKey"].is_string());

        // A relayed signature from an upstream that names no key: the
        // archive, not the key.
        let mut e = entry();
        e.versions[0].signature = Some(SignatureSource::Upstream { public_key: false });
        let doc = extension_json(&e, &urls(), &q(ALL_FLAGS));
        let types: Vec<&str> = doc["versions"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["assetType"].as_str().unwrap())
            .collect();
        assert!(types.contains(&asset_type::SIGNATURE));
        assert!(!types.contains(&asset_type::PUBLIC_KEY));
        let ovsx = openvsx_extension_json(&e, &e.versions[0], &e.versions, &urls());
        assert!(ovsx["files"]["signature"].is_string());
        assert!(ovsx["files"].get("publicKey").is_none());

        // Unsigned: neither, as before (`all_six_asset_types_are_advertised`).
        let ovsx =
            openvsx_extension_json(&entry(), &entry().versions[0], &entry().versions, &urls());
        assert!(ovsx["files"].get("signature").is_none());
    }

    #[test]
    fn version_properties_carry_the_engine_range() {
        let doc = extension_json(&entry(), &urls(), &q(ALL_FLAGS));
        let props = doc["versions"][0]["properties"].as_array().unwrap();
        let engine = props
            .iter()
            .find(|p| p["key"] == property_key::ENGINE)
            .expect("the editor refuses a version whose engine it cannot read");
        assert_eq!(engine["value"], "^1.85.0");
    }

    #[test]
    fn latest_version_only_keeps_the_newest() {
        let doc = extension_json(
            &entry(),
            &urls(),
            &q(ALL_FLAGS | flag::INCLUDE_LATEST_VERSION_ONLY),
        );
        let versions = doc["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0]["version"], "2.0.0");
    }

    /// The flags VS Code actually sends to install by id: `IncludeFiles`,
    /// `IncludeVersionProperties`, `IncludeAssetUri`, `IncludeStatistics`,
    /// `ExcludeNonValidated`, `IncludeCategoryAndTags`, and
    /// `IncludeLatestVersionOnly` — but **not** `IncludeVersions`.
    ///
    /// The editor reads `versions[0].files` without checking, so answering this
    /// with `versions: []` fails the install with
    /// `Cannot read properties of undefined (reading 'files')`. This is the
    /// regression the heavy marketplace test caught with a real editor.
    #[test]
    fn the_flag_set_a_real_editor_sends_still_carries_the_latest_version() {
        const VSCODE_INSTALL_FLAGS: u32 = 950;

        let doc = extension_json(&entry(), &urls(), &q(VSCODE_INSTALL_FLAGS));
        let versions = doc["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 1, "the newest version, not none");
        assert_eq!(versions[0]["version"], "2.0.0");
        assert!(
            versions[0]["files"]
                .as_array()
                .expect("files")
                .iter()
                .any(|f| f["assetType"] == asset_type::VSIX_PACKAGE),
            "the editor downloads the VSIX from this list"
        );
    }

    /// Even a client that asks for nothing gets a version it can install from —
    /// only *how many* versions ship is negotiable.
    #[test]
    fn a_flagless_query_gets_the_latest_version_rather_than_none() {
        let doc = extension_json(&entry(), &urls(), &q(0));
        let versions = doc["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0]["version"], "2.0.0");
    }

    #[test]
    fn include_versions_alone_returns_the_whole_history() {
        let doc = extension_json(&entry(), &urls(), &q(ALL_FLAGS));
        let versions = doc["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 2, "newest first");
        assert_eq!(versions[0]["version"], "2.0.0");
        assert_eq!(versions[1]["version"], "1.0.0");
    }

    #[test]
    fn the_flags_gate_what_is_included() {
        let bare = extension_json(&entry(), &urls(), &q(0));
        assert_eq!(bare["versions"][0]["files"], json!([]));
        assert_eq!(bare["categories"], json!([]));
        assert!(bare.get("statistics").is_none());

        let full = extension_json(&entry(), &urls(), &q(ALL_FLAGS));
        assert_eq!(full["categories"], json!(["Linters"]));
        assert!(full["statistics"].is_array());
    }

    /// The editor reads `flags` with `.indexOf` and the two dates with
    /// `Date.parse`, none of them guarded. A real `code --install-extension`
    /// failed on the missing `flags` with
    /// `Cannot read properties of undefined (reading 'indexOf')`.
    #[test]
    fn the_extension_carries_the_flag_string_and_the_dates() {
        let doc = extension_json(&entry(), &urls(), &q(ALL_FLAGS));

        let flags = doc["flags"].as_str().expect("flags is a string");
        assert!(!flags.is_empty());
        assert!(
            !flags.contains("preview"),
            "this server does not know whether an extension is a preview"
        );
        assert_eq!(doc["lastUpdated"], "2024-03-01T00:00:00+00:00", "newest");
        assert_eq!(doc["releaseDate"], "2024-01-01T00:00:00+00:00", "oldest");
    }

    /// The marketplace knows things this proxy is guessing at, so its own values
    /// win where it sent them.
    #[test]
    fn upstream_flags_and_dates_are_not_overwritten() {
        let mut e = entry();
        e.upstream = Some(json!({
            "extensionName": "tool",
            "publisher": { "publisherName": "acme" },
            "flags": "validated public preview",
            "lastUpdated": "2024-06-01T00:00:00Z",
            "releaseDate": "2020-01-01T00:00:00Z",
        }));

        let doc = extension_json(&e, &urls(), &q(ALL_FLAGS));
        assert_eq!(doc["flags"], "validated public preview");
        assert_eq!(doc["lastUpdated"], "2024-06-01T00:00:00Z");
        assert_eq!(doc["releaseDate"], "2020-01-01T00:00:00Z");
    }

    /// The fidelity guarantee: a key the marketplace sent that this proxy does
    /// not model must survive the round trip.
    #[test]
    fn unmodelled_upstream_fields_survive_rendering() {
        let mut e = entry();
        e.upstream = Some(json!({
            "extensionName": "tool",
            "publisher": { "publisherName": "acme" },
            "deploymentType": 0,
            "someFutureField": { "nested": true },
        }));

        let doc = extension_json(&e, &urls(), &q(ALL_FLAGS));
        assert_eq!(doc["someFutureField"]["nested"], json!(true));
        assert_eq!(doc["deploymentType"], json!(0));
        assert_eq!(
            doc["versions"][0]["assetUri"],
            "https://hub.example.com/proxy/vsx1/vscode/asset/acme/tool/2.0.0",
            "…while the fields the proxy owns are still overwritten"
        );
    }

    /// An upstream `assetUri` must never reach the client — that would route
    /// the download around the cache, the audit trail and the download gate.
    #[test]
    fn an_upstream_asset_uri_is_always_overwritten() {
        let mut e = entry();
        e.upstream = Some(json!({
            "extensionName": "tool",
            "publisher": { "publisherName": "acme" },
            "versions": [{ "version": "2.0.0", "assetUri": "https://marketplace.invalid/x" }],
        }));

        let doc = extension_json(&e, &urls(), &q(ALL_FLAGS));
        let rendered = serde_json::to_string(&doc).unwrap();
        assert!(
            !rendered.contains("marketplace.invalid"),
            "upstream URL leaked: {rendered}"
        );
    }

    #[test]
    fn the_envelope_carries_the_total_count() {
        let doc = query_response_json(&[entry()], 42, &urls(), &q(ALL_FLAGS));
        assert_eq!(
            doc["results"][0]["resultMetadata"][0]["metadataItems"][0]["count"],
            42
        );
        assert_eq!(doc["results"][0]["extensions"].as_array().unwrap().len(), 1);
    }

    /// Every version blocked (or filtered for this identity) means there is
    /// nothing to install, and the editor cannot be told that by an extension
    /// object — it would crash on `versions[0]`. It is told by absence.
    #[test]
    fn an_entry_with_no_visible_version_is_dropped_from_the_envelope() {
        let mut e = entry();
        e.versions.clear();
        let doc = query_response_json(&[e], 1, &urls(), &q(ALL_FLAGS));
        assert_eq!(doc["results"][0]["extensions"], json!([]));
    }

    #[test]
    fn an_empty_result_is_a_well_formed_envelope() {
        let doc = query_response_json(&[], 0, &urls(), &q(ALL_FLAGS));
        assert_eq!(doc["results"][0]["extensions"], json!([]));
        assert_eq!(
            doc["results"][0]["resultMetadata"][0]["metadataItems"][0]["count"],
            0
        );
    }

    // ── matching ─────────────────────────────────────────────────────────────

    fn query_with(json: serde_json::Value) -> GalleryQuery {
        GalleryQuery::from_request(&serde_json::from_value(json).unwrap())
    }

    #[test]
    fn an_exact_name_lookup_does_not_match_a_prefix() {
        let e = entry();
        assert!(e.matches(&query_with(json!({
            "filters": [{ "criteria": [{ "filterType": 7, "value": "ACME.TOOL" }] }]
        }))));
        assert!(
            !e.matches(&query_with(json!({
                "filters": [{ "criteria": [{ "filterType": 7, "value": "acme" }] }]
            }))),
            "a prefix match here would install the wrong extension"
        );
    }

    #[test]
    fn free_text_matches_name_description_tags_and_categories() {
        let e = entry();
        for text in ["acme", "THING", "linter", "tool"] {
            assert!(
                e.matches(&query_with(json!({
                    "filters": [{ "criteria": [{ "filterType": 10, "value": text }] }]
                }))),
                "should match {text}"
            );
        }
        assert!(!e.matches(&query_with(json!({
            "filters": [{ "criteria": [{ "filterType": 10, "value": "python" }] }]
        }))));
    }

    #[test]
    fn facet_criteria_are_anded() {
        let e = entry();
        assert!(e.matches(&query_with(json!({
            "filters": [{ "criteria": [
                { "filterType": 5, "value": "linters" },
                { "filterType": 1, "value": "acme" }
            ]}]
        }))));
        assert!(
            !e.matches(&query_with(json!({
                "filters": [{ "criteria": [
                    { "filterType": 5, "value": "Linters" },
                    { "filterType": 1, "value": "absent" }
                ]}]
            }))),
            "every criterion must hold"
        );
    }

    #[test]
    fn upstream_versions_decode_their_properties() {
        let v = GalleryVersion::from_upstream(&json!({
            "version": "1.2.3",
            "lastUpdated": "2024-05-01T00:00:00Z",
            "properties": [
                { "key": property_key::ENGINE, "value": "^1.90.0" },
                { "key": property_key::EXTENSION_PACK, "value": "a.b, c.d" },
                { "key": property_key::PRE_RELEASE, "value": "true" },
            ],
        }))
        .unwrap();

        assert_eq!(v.engine.as_deref(), Some("^1.90.0"));
        assert_eq!(v.extension_pack, ["a.b", "c.d"], "comma-separated, trimmed");
        assert!(v.pre_release);
    }

    #[test]
    fn an_upstream_object_missing_its_coordinate_is_skipped() {
        assert!(GalleryEntry::from_upstream(&json!({ "displayName": "no name" })).is_none());
    }
}
