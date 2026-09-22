//! Devfile registries — the parts of the protocol that need no I/O (RFC 0035).
//!
//! A devfile registry answers three families of request, and every one of them
//! is expressed here as a coordinate this proxy already knows how to gate:
//!
//! - **The index documents** (`/index`, `/v2index`, each with `/sample`,
//!   `/stack` and `/all`) describe the whole registry. [`index_address`] turns
//!   the request into the upstream-relative path the adapter fetches, with the
//!   query canonicalised so a client cannot mint cache keys; the filters are
//!   [`strip_v2`] and [`strip_legacy`], run through `blocking::dispatch_multi`.
//! - **The OCI objects of a stack version** — its manifest and each layer —
//!   are *artifacts* of the `stack@version` coordinate:
//!
//!   ```text
//!   PackageId { name: "nodejs", version: "2.2.1", artifact: Some("manifest") }
//!   PackageId { name: "nodejs", version: "2.2.1", artifact: Some("layer/devfile.yaml") }
//!   ```
//!
//!   so the block list, the rules and the artifact cache apply to them through
//!   `ProxyService::handle` with nothing new. A request by *digest* is turned
//!   back into one of these coordinates by the web layer, from the filtered
//!   index — a blocked version is not in it, so its digests cannot be reached.
//! - **Starter projects** are artifacts of their version too:
//!   `starter-projects/{name}.zip`.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::CoreError;
use crate::ports::DocumentKind;
use crate::services::blocking::{best_latest, MultiPackageBlocks};

/// The artifact sub-coordinate of a version's OCI manifest.
pub const MANIFEST_ARTIFACT: &str = "manifest";
/// The prefix of a layer's artifact sub-coordinate; the rest is the layer's
/// `org.opencontainers.image.title`.
pub const LAYER_PREFIX: &str = "layer/";
/// The prefix of a starter project's artifact sub-coordinate.
pub const STARTER_PREFIX: &str = "starter-projects/";
/// The layer every stack version has, and the one Che and the REST route serve.
pub const DEVFILE_TITLE: &str = "devfile.yaml";
/// The media types a devfile client accepts for a manifest — the list
/// containerd sends, so upstream answers this proxy as it answers the client.
pub const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.manifest.v1+json, \
    application/vnd.docker.distribution.manifest.v2+json";
/// What the manifest is served as. Upstream serves OCI manifests only, and a
/// manifest *list* is refused when it is parsed ([`manifest_descriptors`]).
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// The OCI annotation a layer's file name travels in.
const TITLE_ANNOTATION: &str = "org.opencontainers.image.title";

/// The upstream path of an index document, and which document it is.
///
/// `/index/stack` is upstream's alias for `/index`, and `/v2index/stack` for
/// `/v2index` (both probed, RFC 0035 §6.5). Anything else is not an index.
pub fn index_document(path: &str) -> Option<DocumentKind> {
    match path {
        "index" | "index/sample" | "index/stack" | "index/all" => Some(DocumentKind::LEGACY_INDEX),
        "v2index" | "v2index/sample" | "v2index/stack" | "v2index/all" => {
            Some(DocumentKind::Versions)
        }
        _ => None,
    }
}

/// The upstream-relative address of one index document with its query
/// canonicalised, and the document kind — the pair `multi_package_document`
/// is asked for, the address travelling as the listing's package name.
///
/// The four parameters `registry-library` sends (`GetRegistryIndex`) are kept,
/// validated with upstream's own patterns and sorted; anything else is dropped
/// before the fetch — upstream ignores it too (`/v2index?foo=bar` answers the
/// plain index, probed) — so the cache key space is bounded by what a client
/// can legitimately ask for.
pub fn index_address(path: &str, query: &str) -> Result<(String, DocumentKind), CoreError> {
    let kind = index_document(path)
        .ok_or_else(|| CoreError::NotFound(format!("'{path}' is not a devfile index document")))?;
    let mut kept: Vec<(&str, &str)> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let valid = match key {
            "arch" => is_arch(value),
            "deprecated" => matches!(value, "true" | "false"),
            "minSchemaVersion" | "maxSchemaVersion" => is_schema_version(value),
            _ => continue,
        };
        if !valid {
            return Err(CoreError::InvalidInput(format!(
                "'{value}' is not a valid value for the devfile index parameter '{key}'"
            )));
        }
        kept.push((key, value));
    }
    kept.sort_unstable();
    kept.dedup();
    let address = if kept.is_empty() {
        path.to_owned()
    } else {
        let q: Vec<String> = kept.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{path}?{}", q.join("&"))
    };
    Ok((address, kind))
}

/// An index address as [`index_address`] produced it — checked again by the
/// adapter before it becomes an upstream URL, because the package name of a
/// listing is the one field a future caller could fill from somewhere else.
pub fn is_index_address(address: &str) -> bool {
    let (path, query) = address.split_once('?').unwrap_or((address, ""));
    matches!(index_address(path, query), Ok((canonical, _)) if canonical == address)
}

/// `arch` values: upstream's are `amd64`, `arm64`, `ppc64le`, `s390x`.
fn is_arch(v: &str) -> bool {
    (1..=16).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Upstream's own pattern, quoted in its `400`:
/// `^[0-9]+\.[0-9]+(\.[0-9]+(\-alpha)?)?$`.
fn is_schema_version(v: &str) -> bool {
    let (numbers, alpha) = match v.strip_suffix("-alpha") {
        Some(n) => (n, true),
        None => (v, false),
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    let numeric = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    match parts.len() {
        2 => !alpha && parts.iter().all(numeric),
        3 => parts.iter().all(numeric),
        _ => false,
    }
}

/// A stack name as upstream spells one (`nodejs`, `java-springboot`,
/// `dotnet80`) — and nothing that could leave a path segment.
pub fn validate_stack(name: &str) -> Result<(), CoreError> {
    let ok = (1..=128).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(CoreError::InvalidInput(format!(
            "'{name}' is not a devfile stack name"
        )))
    }
}

/// A stack version or OCI tag: semver-shaped, without `/` or a leading dot.
pub fn validate_version(version: &str) -> Result<(), CoreError> {
    let ok = (1..=128).contains(&version.len())
        && version
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
        && !version.contains("..");
    if ok {
        Ok(())
    } else {
        Err(CoreError::InvalidInput(format!(
            "'{version}' is not a devfile stack version"
        )))
    }
}

/// A starter project name, as it appears in a devfile's `starterProjects`.
pub fn validate_starter(name: &str) -> Result<(), CoreError> {
    validate_stack(name).map_err(|_| {
        CoreError::InvalidInput(format!("'{name}' is not a devfile starter project name"))
    })
}

/// `sha256:<64 lower-case hex>`, returning the hex. Every digest in this
/// protocol is SHA-256, and the hex is what becomes a checksum and a key.
pub fn validate_digest(digest: &str) -> Result<&str, CoreError> {
    digest
        .strip_prefix("sha256:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| CoreError::InvalidInput(format!("'{digest}' is not a sha256 digest")))
}

/// An OCI repository namespace (`devfile-catalog`): path segments of
/// `[a-z0-9._-]`, as the distribution spec allows, none of them a dot segment.
pub fn validate_namespace(ns: &str) -> Result<(), CoreError> {
    let ok = !ns.is_empty()
        && ns.len() <= 255
        && ns.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
                })
        });
    if ok {
        Ok(())
    } else {
        Err(CoreError::InvalidInput(format!(
            "'{ns}' is not an OCI repository namespace"
        )))
    }
}

/// `links.self` — `devfile-catalog/nodejs:2.2.1` — split into the OCI
/// namespace and tag, checked against the stack it belongs to.
///
/// The namespace is read, never assumed: `devfile-catalog` is what the
/// `registry-support` build tools write, but it is the index that says so.
pub fn self_link(link: &str, stack: &str) -> Result<(String, String), CoreError> {
    let bad = || {
        CoreError::Registry(format!(
            "devfile index link '{link}' is not '{{ns}}/{stack}:{{tag}}'"
        ))
    };
    let (repo, tag) = link.rsplit_once(':').ok_or_else(bad)?;
    let (ns, name) = repo.rsplit_once('/').ok_or_else(bad)?;
    if name != stack {
        return Err(bad());
    }
    validate_namespace(ns).map_err(|_| bad())?;
    validate_version(tag).map_err(|_| bad())?;
    Ok((ns.to_owned(), tag.to_owned()))
}

/// Whether an index entry is a sample. Samples carry a git remote and no
/// version; they are listed and never filtered.
fn is_sample(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("sample")
}

/// A stack's entry in either index shape.
pub fn find_stack<'a>(index: &'a Value, stack: &str) -> Option<&'a Value> {
    index
        .as_array()?
        .iter()
        .find(|e| !is_sample(e) && e.get("name").and_then(Value::as_str) == Some(stack))
}

/// Every version of a v2 index entry, in the index's own order.
pub fn versions_of(entry: &Value) -> Vec<&str> {
    entry
        .get("versions")
        .and_then(Value::as_array)
        .map(|vs| {
            vs.iter()
                .filter_map(|v| v.get("version").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

/// One version's record in a v2 index entry.
pub fn version_entry<'a>(entry: &'a Value, version: &str) -> Option<&'a Value> {
    entry
        .get("versions")?
        .as_array()?
        .iter()
        .find(|v| v.get("version").and_then(Value::as_str) == Some(version))
}

/// The version a request without one resolves to — `default: true`, else the
/// first — as `registry-library`'s `GetStackLink` resolves it.
pub fn default_version(entry: &Value) -> Option<&str> {
    let versions = entry.get("versions")?.as_array()?;
    versions
        .iter()
        .find(|v| v.get("default").and_then(Value::as_bool) == Some(true))
        .or_else(|| versions.first())
        .and_then(|v| v.get("version"))
        .and_then(Value::as_str)
}

/// The starter projects a v2 version record names.
pub fn starter_projects(version: &Value) -> Vec<&str> {
    version
        .get("starterProjects")
        .and_then(Value::as_array)
        .map(|s| s.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Remove blocked versions from a **v2** index.
///
/// A stack loses the blocked records from `versions`, and a stack with none
/// left loses its entry. When the removed record was the default, `default`
/// moves to the highest version left — RFC 0035 §11 question 1, answered as
/// recommended: the block already changed what can be installed, and without
/// a default an unpinned `pull` fails with a message that blames the registry
/// rather than the policy. Samples are never touched.
///
/// Returns the `name@version` pairs removed.
pub fn strip_v2(doc: &mut Value, blocked: &MultiPackageBlocks) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(entries) = doc.as_array_mut() else {
        return removed;
    };
    entries.retain_mut(|entry| {
        if is_sample(entry) {
            return true;
        }
        let Some(name) = entry.get("name").and_then(Value::as_str).map(str::to_owned) else {
            return true;
        };
        let Some(versions) = entry.get_mut("versions").and_then(Value::as_array_mut) else {
            return true;
        };
        let mut lost_default = false;
        versions.retain(|v| {
            let Some(version) = v.get("version").and_then(Value::as_str) else {
                return true;
            };
            if !blocked.contains(&name, version) {
                return true;
            }
            lost_default |= v.get("default").and_then(Value::as_bool) == Some(true);
            removed.push(format!("{name}@{version}"));
            false
        });
        if versions.is_empty() {
            return false;
        }
        if lost_default {
            let left: Vec<String> = versions
                .iter()
                .filter_map(|v| v.get("version").and_then(Value::as_str).map(str::to_owned))
                .collect();
            if let Some(best) = best_latest(&left) {
                for v in versions.iter_mut() {
                    if v.get("version").and_then(Value::as_str) == Some(best.as_str()) {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("default".to_owned(), Value::Bool(true));
                        }
                    }
                }
            }
        }
        true
    });
    removed
}

/// Remove stacks whose one listed version is blocked from a **legacy** index.
///
/// The legacy entry names one version, the default; if that is blocked the
/// entry goes, rather than being rewritten to another version whose fields it
/// does not carry (RFC 0035 §11 decision 2).
pub fn strip_legacy(doc: &mut Value, blocked: &MultiPackageBlocks) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(entries) = doc.as_array_mut() else {
        return removed;
    };
    entries.retain(|entry| {
        if is_sample(entry) {
            return true;
        }
        let name = entry.get("name").and_then(Value::as_str);
        let version = entry.get("version").and_then(Value::as_str);
        match (name, version) {
            (Some(n), Some(v)) if blocked.contains(n, v) => {
                removed.push(format!("{n}@{v}"));
                false
            }
            _ => true,
        }
    });
    removed
}

/// One descriptor of an OCI manifest: its config or one of its layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub media_type: String,
    /// `sha256:<hex>`, validated.
    pub digest: String,
    pub size: u64,
    /// The layer's file name (`devfile.yaml`); `None` for the config.
    pub title: Option<String>,
}

impl Descriptor {
    /// The bare hex of the digest — the shape `PackageMetadata::checksum`
    /// must have, or cache-write verification silently skips itself.
    pub fn hex(&self) -> &str {
        self.digest.strip_prefix("sha256:").unwrap_or(&self.digest)
    }
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(rename = "mediaType")]
    media_type: Option<String>,
    config: Option<RawDescriptor>,
    layers: Option<Vec<RawDescriptor>>,
    manifests: Option<Value>,
}

#[derive(Deserialize)]
struct RawDescriptor {
    #[serde(rename = "mediaType", default)]
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    annotations: std::collections::BTreeMap<String, String>,
}

/// The config and layer descriptors of a stack version's manifest.
///
/// A manifest *list* (an image index) is refused — upstream serves none for a
/// stack, and choosing a platform is not a thing a devfile client does. Every
/// digest is validated here, before it can become a storage key or a URL.
pub fn manifest_descriptors(bytes: &[u8]) -> Result<Vec<Descriptor>, CoreError> {
    let raw: RawManifest = serde_json::from_slice(bytes)
        .map_err(|e| CoreError::Registry(format!("devfile OCI manifest does not parse: {e}")))?;
    if raw.manifests.is_some()
        || raw
            .media_type
            .as_deref()
            .is_some_and(|m| m.contains("index") || m.contains("list"))
    {
        return Err(CoreError::Registry(
            "devfile OCI manifest is an image index, not a stack manifest".to_owned(),
        ));
    }
    let mut out = Vec::new();
    for (d, is_config) in raw.config.into_iter().map(|d| (d, true)).chain(
        raw.layers
            .unwrap_or_default()
            .into_iter()
            .map(|d| (d, false)),
    ) {
        validate_digest(&d.digest).map_err(|e| CoreError::Registry(e.to_string()))?;
        let title = if is_config {
            None
        } else {
            let title = d.annotations.get(TITLE_ANNOTATION).cloned();
            if let Some(t) = &title {
                if t.is_empty() || t.contains('/') || t.contains("..") {
                    return Err(CoreError::Registry(format!(
                        "devfile OCI layer title '{t}' is not a file name"
                    )));
                }
            }
            title
        };
        out.push(Descriptor {
            media_type: d.media_type,
            digest: d.digest,
            size: d.size,
            title,
        });
    }
    Ok(out)
}

/// The artifact sub-coordinate a layer is cached under.
pub fn layer_artifact(title: &str) -> String {
    format!("{LAYER_PREFIX}{title}")
}

/// The artifact sub-coordinate a starter project is cached under.
pub fn starter_artifact(name: &str) -> String {
    format!("{STARTER_PREFIX}{name}.zip")
}

/// The starter project an artifact sub-coordinate names, if it names one.
pub fn starter_of(artifact: &str) -> Option<&str> {
    artifact.strip_prefix(STARTER_PREFIX)?.strip_suffix(".zip")
}

/// The OCI namespace a disconnected instance names in `links.self` when the
/// held facts carry none: the one `registry-support`'s build tools write, so a
/// bundle made from `registry.devfile.io` composes the links its clients had.
pub const DEFAULT_NAMESPACE: &str = "devfile-catalog";

/// What a bundle import reads off a devfile artifact's bytes, keyed by kind
/// the way every import fact is (RFC 0008-bis §13.4): `{"devfile": {…}}`.
///
/// - a **manifest** gives `manifestDigest` and `layers` — what the OCI routes
///   look a digest up in, and what `resources` lists;
/// - a **`devfile.yaml`** gives `metadata`, `schemaVersion` and
///   `starterProjects` — what a composed index entry says about the version.
///
/// The adapter files the same shape under `extra.devfile` on a connected
/// instance, so the web layer and the composition read one place whichever
/// side wrote it. `Null` for any other artifact.
///
/// `artifact` is `None` when the import knows the entry by package and
/// version only, which is how a bundle names it; the file is then recognised
/// from its bytes ([`sniff_artifact`]).
pub fn import_facts(artifact: Option<&str>, bytes: &[u8]) -> Value {
    let sniffed;
    let artifact = match artifact {
        Some(a) => Some(a),
        None => {
            sniffed = sniff_artifact(bytes);
            sniffed.as_deref()
        }
    };
    let facts = match artifact {
        Some(MANIFEST_ARTIFACT) => manifest_descriptors(bytes).ok().map(|d| {
            use sha2::Digest;
            serde_json::json!({
                "manifestDigest": format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes))),
                "layers": layers_json(&d),
            })
        }),
        Some(a) if a == format!("{LAYER_PREFIX}{DEVFILE_TITLE}") => {
            std::str::from_utf8(bytes).ok().map(devfile_facts)
        }
        _ => None,
    };
    facts.map_or(Value::Null, |f| serde_json::json!({ "devfile": f }))
}

/// Which file of a stack version these bytes are, when nothing else says: a
/// manifest parses as one, and a devfile is text with no NUL byte and a
/// `schemaVersion:` line at column 0. Anything else — `archive.tar`, an icon —
/// is `None` and carries no facts.
fn sniff_artifact(bytes: &[u8]) -> Option<String> {
    if manifest_descriptors(bytes).is_ok() {
        return Some(MANIFEST_ARTIFACT.to_owned());
    }
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0') && text.lines().any(|l| l.starts_with("schemaVersion:")))
        .then(|| layer_artifact(DEVFILE_TITLE))
}

/// The layer descriptors as the facts carry them: titled layers only.
pub fn layers_json(descriptors: &[Descriptor]) -> Value {
    descriptors
        .iter()
        .filter(|d| d.title.is_some())
        .map(|d| {
            serde_json::json!({
                "title": d.title, "digest": d.digest, "size": d.size, "mediaType": d.media_type,
            })
        })
        .collect()
}

/// `metadata`, `schemaVersion` and the starter-project names of a devfile.
///
/// A deliberately narrow reader rather than a YAML parser — the workspace has
/// none, and the one field set a composed index needs sits in the same shape in
/// every devfile `registry-support` builds: `schemaVersion:` and `metadata:` at
/// column 0, the metadata's scalars two spaces in, a list's items four spaces
/// in as `- item`, and each starter project a `- name:` two spaces in. A value
/// in any other shape is left out, never guessed, and the composition falls
/// back to the stack name for `displayName` and to empty `tags`.
// ponytail: line-based subset of YAML; swap for a real parser if a devfile in
// the wild carries its metadata as a flow mapping or a multi-line scalar.
fn devfile_facts(text: &str) -> Value {
    let mut metadata = Map::new();
    let mut schema_version = None;
    let mut starters: Vec<String> = Vec::new();
    let mut section = "";
    let mut list_key: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let body = line.trim();
        if indent == 0 {
            list_key = None;
            section = match body.split_once(':') {
                Some(("metadata", "")) => "metadata",
                Some(("starterProjects", "")) => "starterProjects",
                Some(("schemaVersion", v)) => {
                    schema_version = Some(scalar(v));
                    ""
                }
                _ => "",
            };
            continue;
        }
        match section {
            "metadata" if indent == 2 => match body.split_once(':') {
                Some((key, "")) => {
                    metadata.insert(key.to_owned(), Value::Array(Vec::new()));
                    list_key = Some(key.to_owned());
                }
                Some((key, v)) => {
                    metadata.insert(key.to_owned(), Value::String(scalar(v)));
                    list_key = None;
                }
                None => list_key = None,
            },
            "metadata" if indent >= 4 => {
                if let (Some(key), Some(item)) = (&list_key, body.strip_prefix("- ")) {
                    if let Some(Value::Array(items)) = metadata.get_mut(key) {
                        items.push(Value::String(scalar(item)));
                    }
                }
            }
            "starterProjects" if indent == 2 => {
                if let Some(name) = body.strip_prefix("- name:") {
                    starters.push(scalar(name));
                }
            }
            _ => {}
        }
    }
    serde_json::json!({
        "metadata": metadata,
        "schemaVersion": schema_version,
        "starterProjects": starters,
    })
}

/// A YAML plain or quoted scalar, as the reader above meets one.
fn scalar(raw: &str) -> String {
    let v = raw.trim();
    let quoted = v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')));
    if quoted {
        v[1..v.len() - 1].to_owned()
    } else {
        v.to_owned()
    }
}

/// The held facts of one stack version, merged across the rows that hold it
/// (the manifest's and the devfile's are two artifacts, two `meta:` entries).
#[derive(Debug, Default, Clone)]
pub struct HeldStackVersion {
    pub version: String,
    pub facts: Map<String, Value>,
}

/// The two index documents a disconnected instance composes from what it
/// holds (RFC 0035 §6.8). `address` is the listing's canonical address —
/// `v2index/all?arch=amd64` — so the variant and the query come from it.
///
/// `held` is `(stack, version facts)` for every held version. Samples are
/// never composed: their source is a git remote the air gap does not have.
/// The default is the highest held version. `icon` is always present and
/// empty: Che's `isDevfileMetaData` drops an entry whose icon is undefined,
/// and the upstream icon is a URL a disconnected browser cannot reach.
pub fn compose_index(address: &str, legacy: bool, held: &[(String, HeldStackVersion)]) -> Value {
    let (path, query) = address.split_once('?').unwrap_or((address, ""));
    if path.ends_with("/sample") {
        return Value::Array(Vec::new());
    }
    let archs: Vec<&str> = query
        .split('&')
        .filter_map(|p| p.strip_prefix("arch="))
        .collect();
    // Upstream's `FilterDevfileDeprecated` judges a whole stack by its default
    // version's tags: `false` drops deprecated stacks, `true` keeps only them.
    let want_deprecated = query.split('&').find_map(|p| match p {
        "deprecated=true" => Some(true),
        "deprecated=false" => Some(false),
        _ => None,
    });

    let mut stacks: std::collections::BTreeMap<&str, Vec<&HeldStackVersion>> = Default::default();
    for (stack, v) in held {
        stacks.entry(stack.as_str()).or_default().push(v);
    }
    let mut out = Vec::new();
    for (stack, versions) in stacks {
        let mut records: Vec<(String, Value)> = versions
            .iter()
            .filter(|v| {
                let meta = v.facts.get("metadata");
                let list = |k: &str| -> Vec<&str> {
                    meta.and_then(|m| m.get(k))
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default()
                };
                archs.is_empty() || {
                    let have = list("architectures");
                    have.is_empty() || archs.iter().all(|a| have.contains(a))
                }
            })
            .map(|v| (v.version.clone(), version_record(stack, v)))
            .collect();
        if records.is_empty() {
            continue;
        }
        let names: Vec<String> = records.iter().map(|(v, _)| v.clone()).collect();
        let Some(default) = best_latest(&names) else {
            continue;
        };
        records.sort_by(|a, b| crate::services::version_order::newest_first(&a.0, &b.0));
        let top = versions
            .iter()
            .find(|v| v.version == default)
            .copied()
            .cloned()
            .unwrap_or_default();
        let meta = top.facts.get("metadata").cloned().unwrap_or_default();
        let deprecated = meta
            .get("tags")
            .and_then(Value::as_array)
            .is_some_and(|t| t.iter().any(|t| t.as_str() == Some("Deprecated")));
        if want_deprecated.is_some_and(|want| want != deprecated) {
            continue;
        }
        let field = |k: &str| meta.get(k).cloned();
        let mut entry = Map::new();
        entry.insert("name".into(), Value::String(stack.to_owned()));
        entry.insert(
            "displayName".into(),
            field("displayName").unwrap_or_else(|| Value::String(stack.to_owned())),
        );
        if let Some(d) = field("description") {
            entry.insert("description".into(), d);
        }
        entry.insert("type".into(), Value::String("stack".into()));
        entry.insert(
            "tags".into(),
            field("tags").unwrap_or_else(|| Value::Array(Vec::new())),
        );
        entry.insert("icon".into(), Value::String(String::new()));
        for k in ["projectType", "language", "provider"] {
            if let Some(v) = field(k) {
                entry.insert(k.into(), v);
            }
        }
        if legacy {
            let record = records
                .iter()
                .find(|(v, _)| *v == default)
                .map(|(_, r)| r.clone())
                .unwrap_or_default();
            entry.insert("version".into(), Value::String(default.clone()));
            for k in ["links", "resources", "starterProjects"] {
                if let Some(v) = record.get(k) {
                    entry.insert(k.into(), v.clone());
                }
            }
        } else {
            let versions: Vec<Value> = records
                .into_iter()
                .map(|(v, mut r)| {
                    if v == default {
                        r["default"] = Value::Bool(true);
                    }
                    r
                })
                .collect();
            entry.insert("versions".into(), Value::Array(versions));
        }
        out.push(Value::Object(entry));
    }
    Value::Array(out)
}

/// One version's record in a composed v2 index.
fn version_record(stack: &str, v: &HeldStackVersion) -> Value {
    let f = &v.facts;
    let meta = f.get("metadata").cloned().unwrap_or_default();
    let ns = f
        .get("namespace")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_NAMESPACE);
    let resources: Vec<Value> = f
        .get("layers")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(|x| x.get("title").cloned()).collect())
        .filter(|r: &Vec<Value>| !r.is_empty())
        .unwrap_or_else(|| vec![Value::String(DEVFILE_TITLE.into())]);
    let mut r = serde_json::json!({
        "version": v.version,
        "links": { "self": format!("{ns}/{stack}:{}", v.version) },
        "resources": resources,
        "starterProjects": f.get("starterProjects").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
        "tags": meta.get("tags").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
    });
    for (to, from) in [
        ("schemaVersion", f.get("schemaVersion")),
        ("description", meta.get("description")),
        ("architectures", meta.get("architectures")),
    ] {
        if let Some(v) = from.filter(|v| !v.is_null()) {
            r[to] = v.clone();
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;
    use serde_json::json;

    fn blocks(pairs: &[(&str, &str)]) -> MultiPackageBlocks {
        MultiPackageBlocks::new(
            RegistryKind::Devfile,
            pairs
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn v2() -> Value {
        json!([
            {"name": "nodejs", "type": "stack", "versions": [
                {"version": "2.2.1", "default": true, "links": {"self": "devfile-catalog/nodejs:2.2.1"}},
                {"version": "2.2.0", "links": {"self": "devfile-catalog/nodejs:2.2.0"}},
                {"version": "2.1.1", "links": {"self": "devfile-catalog/nodejs:2.1.1"}}
            ]},
            {"name": "go", "type": "stack", "versions": [
                {"version": "2.6.0", "default": true}
            ]},
            {"name": "nodejs-basic", "type": "sample", "git": {"remotes": {"origin": "x"}}}
        ])
    }

    #[test]
    fn index_paths_map_to_their_document() {
        assert_eq!(index_document("v2index/all"), Some(DocumentKind::Versions));
        assert_eq!(
            index_document("index/stack"),
            Some(DocumentKind::LEGACY_INDEX)
        );
        assert_eq!(index_document("index/foo"), None);
        assert_eq!(index_document("../index"), None);
    }

    #[test]
    fn the_query_is_validated_sorted_and_stripped_of_unknown_keys() {
        let (addr, kind) =
            index_address("v2index", "deprecated=false&foo=bar&arch=arm64&arch=amd64").unwrap();
        assert_eq!(addr, "v2index?arch=amd64&arch=arm64&deprecated=false");
        assert_eq!(kind, DocumentKind::Versions);
        assert_eq!(index_address("index", "").unwrap().0, "index");
        assert!(is_index_address(
            "v2index?arch=amd64&arch=arm64&deprecated=false"
        ));
        assert!(!is_index_address("v2index?foo=bar"));
        assert!(!is_index_address("../v2index"));
    }

    #[test]
    fn schema_versions_follow_upstreams_pattern() {
        for ok in ["2.2", "2.2.0", "2.2.0-alpha", "10.0.1"] {
            assert!(is_schema_version(ok), "{ok}");
        }
        for bad in ["zz", "2", "2.2-alpha", "2.2.0.1", "2..0", "2.2.0-beta"] {
            assert!(!is_schema_version(bad), "{bad}");
        }
        assert!(index_address("v2index", "minSchemaVersion=zz").is_err());
        assert!(index_address("v2index", "arch=../x").is_err());
    }

    #[test]
    fn self_link_is_split_and_checked_against_its_stack() {
        assert_eq!(
            self_link("devfile-catalog/nodejs:2.2.1", "nodejs").unwrap(),
            ("devfile-catalog".to_owned(), "2.2.1".to_owned())
        );
        assert!(self_link("devfile-catalog/go:2.2.1", "nodejs").is_err());
        assert!(self_link("../nodejs:2.2.1", "nodejs").is_err());
        assert!(self_link("devfile-catalog/nodejs:../x", "nodejs").is_err());
        assert!(self_link("nodejs", "nodejs").is_err());
    }

    #[test]
    fn coordinates_refuse_traversal() {
        assert!(validate_stack("java-springboot").is_ok());
        assert!(validate_stack("..").is_err());
        assert!(validate_stack("a/b").is_err());
        assert!(validate_version("2.2.1").is_ok());
        assert!(validate_version("../../etc/x").is_err());
        assert!(validate_version("1..2").is_err());
        let hex = "e".repeat(64);
        assert_eq!(validate_digest(&format!("sha256:{hex}")).unwrap(), hex);
        assert!(validate_digest("sha256:../../x").is_err());
        assert!(validate_digest(&format!("sha256:{}", "E".repeat(64))).is_err());
    }

    #[test]
    fn nothing_blocked_changes_nothing() {
        let mut doc = v2();
        assert!(strip_v2(&mut doc, &blocks(&[])).is_empty());
        assert_eq!(doc, v2());
    }

    #[test]
    fn a_blocked_version_leaves_the_v2_index_and_the_default_moves() {
        let mut doc = v2();
        let removed = strip_v2(&mut doc, &blocks(&[("nodejs", "2.2.1")]));
        assert_eq!(removed, vec!["nodejs@2.2.1"]);
        let node = find_stack(&doc, "nodejs").unwrap();
        assert_eq!(versions_of(node), vec!["2.2.0", "2.1.1"]);
        assert_eq!(default_version(node), Some("2.2.0"));
        assert_eq!(
            version_entry(node, "2.2.0").unwrap()["default"],
            json!(true)
        );
    }

    #[test]
    fn a_stack_with_every_version_blocked_is_removed_and_samples_stay() {
        let mut doc = v2();
        strip_v2(&mut doc, &blocks(&[("go", "2.6.0"), ("nodejs-basic", "x")]));
        assert!(find_stack(&doc, "go").is_none());
        assert_eq!(doc.as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_legacy_index_drops_a_stack_whose_default_is_blocked() {
        let mut doc = json!([
            {"name": "nodejs", "type": "stack", "version": "2.2.1"},
            {"name": "go", "type": "stack", "version": "2.6.0"},
            {"name": "nodejs-basic", "type": "sample"}
        ]);
        assert!(strip_legacy(&mut doc, &blocks(&[("nodejs", "2.2.0")])).is_empty());
        assert_eq!(doc.as_array().unwrap().len(), 3);
        assert_eq!(
            strip_legacy(&mut doc, &blocks(&[("nodejs", "2.2.1")])),
            vec!["nodejs@2.2.1"]
        );
        assert!(find_stack(&doc, "nodejs").is_none());
        assert_eq!(doc.as_array().unwrap().len(), 2);
    }

    /// The manifest `registry.devfile.io` served for `go@2.6.0` on 2026-09-22.
    const GO_MANIFEST: &str = r#"{"schemaVersion":2,"config":{"mediaType":"application/vnd.devfileio.devfile.config.v2+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[{"mediaType":"application/x-tar","digest":"sha256:5144d54c15fe1b17eb6a27ac1f71c4709b9527fdab38ae925642601c7bb00a1e","size":617,"annotations":{"org.opencontainers.image.title":"archive.tar"}},{"mediaType":"application/vnd.devfileio.devfile.layer.v1","digest":"sha256:96a8fb6c37ab081b247bc8cebb3eb44c7076ff8d01cf39744a480e7f5e7f9548","size":2571,"annotations":{"org.opencontainers.image.title":"devfile.yaml"}}]}"#;

    #[test]
    fn a_real_manifest_yields_its_config_and_titled_layers() {
        let d = manifest_descriptors(GO_MANIFEST.as_bytes()).unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].title, None);
        assert_eq!(d[1].title.as_deref(), Some("archive.tar"));
        assert_eq!(d[2].title.as_deref(), Some("devfile.yaml"));
        assert_eq!(d[2].size, 2571);
        assert_eq!(
            d[2].hex(),
            "96a8fb6c37ab081b247bc8cebb3eb44c7076ff8d01cf39744a480e7f5e7f9548"
        );
    }

    #[test]
    fn a_manifest_with_a_bad_digest_title_or_an_index_is_refused() {
        let bad_digest = GO_MANIFEST.replace("sha256:5144", "sha256:../x");
        assert!(manifest_descriptors(bad_digest.as_bytes()).is_err());
        let bad_title = GO_MANIFEST.replace("\"archive.tar\"", "\"../archive.tar\"");
        assert!(manifest_descriptors(bad_title.as_bytes()).is_err());
        let index = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        assert!(manifest_descriptors(index.as_bytes()).is_err());
    }

    /// The head of `registry.devfile.io`'s `nodejs@2.2.1` devfile, as served.
    const NODE_DEVFILE: &str = "schemaVersion: 2.2.2\nmetadata:\n  name: nodejs\n  displayName: Node.js Runtime\n  description: Node.js 18 application\n  icon: https://raw.githubusercontent.com/devfile-samples/devfile-stack-icons/main/node-js.svg\n  tags:\n    - Node.js\n    - Express\n    - ubi8\n  projectType: Node.js\n  language: JavaScript\n  version: 2.2.1\nstarterProjects:\n  - name: nodejs-starter\n    git:\n      checkoutFrom:\n        revision: main\n      remotes:\n        origin: 'https://github.com/nodeshift-starters/devfile-sample.git'\ncomponents:\n  - name: runtime\n    container:\n      image: registry.access.redhat.com/ubi8/nodejs-18:1-32\n";

    #[test]
    fn a_devfile_yields_its_metadata_schema_and_starters() {
        let facts = import_facts(Some("layer/devfile.yaml"), NODE_DEVFILE.as_bytes());
        let f = &facts["devfile"];
        assert_eq!(f["schemaVersion"], "2.2.2");
        assert_eq!(f["metadata"]["displayName"], "Node.js Runtime");
        assert_eq!(f["metadata"]["tags"], json!(["Node.js", "Express", "ubi8"]));
        assert_eq!(f["starterProjects"], json!(["nodejs-starter"]));
        assert!(
            f["metadata"].get("image").is_none(),
            "components are not metadata"
        );
    }

    #[test]
    fn a_manifest_yields_its_digest_and_layers_and_anything_else_nothing() {
        let facts = import_facts(Some("manifest"), GO_MANIFEST.as_bytes());
        assert!(facts["devfile"]["manifestDigest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(facts["devfile"]["layers"].as_array().unwrap().len(), 2);
        assert_eq!(import_facts(Some("layer/archive.tar"), b"x"), Value::Null);
        assert_eq!(import_facts(None, b"x"), Value::Null);
    }

    /// A bundle names its entries by package and version, not by file, so the
    /// import recognises the two files that carry facts from their bytes.
    #[test]
    fn with_no_artifact_the_file_is_recognised_from_its_bytes() {
        let m = import_facts(None, GO_MANIFEST.as_bytes());
        assert!(m["devfile"]["manifestDigest"].is_string(), "{m}");
        let d = import_facts(None, NODE_DEVFILE.as_bytes());
        assert_eq!(d["devfile"]["metadata"]["displayName"], "Node.js Runtime");
        let tar = b"docker/Dockerfile\0\0\0schemaVersion: 2.2.0\n";
        assert_eq!(
            import_facts(None, tar),
            Value::Null,
            "a tar is not a devfile"
        );
    }

    fn held(stack: &str, version: &str, tags: &[&str]) -> (String, HeldStackVersion) {
        let mut facts = Map::new();
        facts.insert(
            "metadata".into(),
            json!({ "displayName": format!("{stack} runtime"), "tags": tags }),
        );
        (
            stack.to_owned(),
            HeldStackVersion {
                version: version.to_owned(),
                facts,
            },
        )
    }

    #[test]
    fn a_composed_v2_index_lists_what_is_held_with_the_highest_as_default() {
        let doc = compose_index(
            "v2index/all",
            false,
            &[
                held("nodejs", "2.2.0", &[]),
                held("nodejs", "2.2.1", &[]),
                held("go", "2.6.0", &[]),
            ],
        );
        let node = find_stack(&doc, "nodejs").unwrap();
        assert_eq!(versions_of(node), vec!["2.2.1", "2.2.0"]);
        assert_eq!(default_version(node), Some("2.2.1"));
        assert_eq!(node["icon"], "", "Che drops an entry with no icon at all");
        assert_eq!(node["displayName"], "nodejs runtime");
        assert_eq!(
            version_entry(node, "2.2.0").unwrap()["links"]["self"],
            "devfile-catalog/nodejs:2.2.0"
        );
    }

    #[test]
    fn a_composed_legacy_index_names_the_default_and_samples_are_empty() {
        let doc = compose_index(
            "index/all",
            true,
            &[held("nodejs", "2.2.0", &[]), held("nodejs", "2.2.1", &[])],
        );
        let node = find_stack(&doc, "nodejs").unwrap();
        assert_eq!(node["version"], "2.2.1");
        assert_eq!(node["links"]["self"], "devfile-catalog/nodejs:2.2.1");
        assert_eq!(
            compose_index("index/sample", true, &[held("nodejs", "2.2.1", &[])]),
            json!([])
        );
    }

    #[test]
    fn the_deprecated_filter_judges_a_stack_by_its_default_version() {
        let held = [
            // A deprecated old version does not deprecate a live stack.
            held("nodejs", "2.1.1", &["Deprecated"]),
            held("nodejs", "2.2.1", &[]),
            held("python", "1.0.0", &["Deprecated"]),
        ];
        let live = compose_index("v2index?deprecated=false", false, &held);
        assert_eq!(
            versions_of(find_stack(&live, "nodejs").unwrap()),
            vec!["2.2.1", "2.1.1"]
        );
        assert!(find_stack(&live, "python").is_none());
        let only = compose_index("v2index?deprecated=true", false, &held);
        assert!(find_stack(&only, "nodejs").is_none());
        assert!(find_stack(&only, "python").is_some());
    }

    #[test]
    fn artifact_names_round_trip() {
        assert_eq!(layer_artifact("devfile.yaml"), "layer/devfile.yaml");
        assert_eq!(
            starter_artifact("nodejs-starter"),
            "starter-projects/nodejs-starter.zip"
        );
        assert_eq!(
            starter_of("starter-projects/nodejs-starter.zip"),
            Some("nodejs-starter")
        );
        assert_eq!(starter_of("layer/devfile.yaml"), None);
    }
}
