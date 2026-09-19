//! Ansible Galaxy's spellings, and the two documents this instance composes
//! (RFC 0031 §6.2).
//!
//! No I/O. Everything here is a parse or a render, so the handler, the adapter
//! and the local registry all agree on one spelling of a collection, one
//! artifact filename and one listing shape.
//!
//! The three facts this module exists to keep in one place:
//!
//! - **A collection is `{namespace}.{name}` to a person and `{namespace}/{name}`
//!   in a URL.** The dotted form is what a `requirements.yml` writes and what an
//!   operator types into a block, so it is what this instance stores; the URL
//!   form is parsed back into it at the edge.
//! - **The artifact is `{ns}-{name}-{version}.tar.gz`.** `ansible-galaxy`'s
//!   `_download_file` derives its working filename by slicing `.tar.gz` off the
//!   last path segment of `download_url`, so the served path has to end in that
//!   exact name rather than in a synthetic `…/versions/{v}/tarball`.
//! - **A listing this instance serves is one page.** Every pagination link the
//!   client follows loses a path prefix — collections `urljoin` an absolute-path
//!   link against the configured api_server, roles join it against a
//!   deliberately stripped `scheme://netloc/` — so a `next` this instance emits
//!   could never be followed back to it. [`compose_versions`] therefore renders
//!   `links` with every member null and `meta.count` describing the one page
//!   served.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::CoreError;

/// How much of Ansible Galaxy's v1 role surface a `galaxy` registry serves
/// (RFC 0031 §4.4).
///
/// An enum rather than a string, so a misspelling is a config error at load
/// rather than a silent fall back to a default the operator did not choose.
///
/// It lives in `core` rather than in `config` because it is read on **every**
/// role request off `HotConfig`, the way `deny_components` is: baked into the
/// registry client instead, a reload would not take effect until the cached
/// upstream document expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GalaxyRoleMode {
    /// Serve the v1 reads and route role bytes through this instance.
    #[default]
    Proxy,
    /// Serve the v1 reads and relay `download_url` as upstream gave it.
    Index,
    /// Serve no v1 surface, and say so in the discovery document.
    Off,
}

impl GalaxyRoleMode {
    /// Whether the v1 endpoints exist at all — which is also what the discovery
    /// document advertises.
    pub fn serves_v1(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether this instance fetches a role's archive itself.
    pub fn proxies_bytes(self) -> bool {
        matches!(self, Self::Proxy)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Index => "index",
            Self::Off => "off",
        }
    }
}

impl std::fmt::Display for GalaxyRoleMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `roles/` prefix a role's package name carries.
///
/// Two namespaces that can hold the same word must not collide in one package
/// table — the reason Terraform's explore names carry `modules/` and
/// `providers/`. `community.general` the collection and `community.general` the
/// role are different things, and an operator blocking one must not block the
/// other.
pub const ROLE_PREFIX: &str = "roles/";

/// Split `{namespace}.{name}` into its halves.
///
/// `^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$` — the rule every real galaxy server
/// enforces on publish. `ansible-galaxy`'s own client-side check
/// (`is_valid_collection_name`) is looser: exactly one dot, both halves Python
/// identifiers and not keywords, which admits `Foo.Bar`. The stricter rule is
/// the safe direction for a string that becomes a storage key, and it is the
/// only one an upstream would have accepted in the first place.
pub fn parse_collection(package: &str) -> Result<(&str, &str), CoreError> {
    let mut parts = package.split('.');
    let (Some(namespace), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(CoreError::InvalidInput(format!(
            "'{package}' is not a collection name: expected '{{namespace}}.{{name}}'"
        )));
    };
    for half in [namespace, name] {
        if !is_galaxy_identifier(half) {
            return Err(CoreError::InvalidInput(format!(
                "'{package}' is not a collection name: '{half}' is not \
                 [a-z][a-z0-9_]*"
            )));
        }
    }
    Ok((namespace, name))
}

/// The dotted package name for a `{namespace}/{name}` pair taken from a URL.
///
/// Validated on the way through, so a traversal segment becomes a `400` at the
/// edge rather than a storage key — the Maven and NuGet-flat rule, because the
/// galaxy handlers build their keys themselves.
pub fn collection_package(namespace: &str, name: &str) -> Result<String, CoreError> {
    let package = format!("{namespace}.{name}");
    parse_collection(&package)?;
    Ok(package)
}

/// A role's package name: `roles/{github_user}.{role_name}`.
///
/// Role names are not held to the collection rule — galaxy.ansible.com carries
/// roles whose GitHub user has upper-case letters and dashes — so this
/// validates the narrower thing that matters: no separator, no traversal, and
/// nothing that is not a plausible name segment.
pub fn role_key(user: &str, role: &str) -> Result<String, CoreError> {
    for half in [user, role] {
        if half.is_empty()
            || !half
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            || half.contains("..")
        {
            return Err(CoreError::InvalidInput(format!(
                "'{half}' is not a role name segment"
            )));
        }
    }
    Ok(format!("{ROLE_PREFIX}{user}.{role}"))
}

/// The **alias** spelling of a role, for a request that arrived under the
/// numeric id upstream addresses it by: `roles/#{id}`.
///
/// `ansible-galaxy` learns a role's id from the search document and then asks
/// for `v1/roles/{id}/versions/`, so the id is the only thing the second
/// request carries. Passing it through would cache and block under a spelling
/// no operator ever types — the JetBrains Marketplace numeric-id problem, one
/// protocol over — so it is resolved to `roles/{user}.{role}` through
/// `RegistryClient::canonical_coordinate` before anything else runs.
///
/// `#` cannot appear in a role name, so [`role_id_of`] is unambiguous.
pub fn role_alias(id: &str) -> String {
    format!("{ROLE_PREFIX}#{id}")
}

/// The numeric id a [`role_alias`] carries, or `None` for a named role.
pub fn role_id_of(package: &str) -> Option<&str> {
    package.strip_prefix(ROLE_PREFIX)?.strip_prefix('#')
}

/// Whether a package name addresses the v1 role surface rather than a
/// collection.
pub fn is_role(package: &str) -> bool {
    package.starts_with(ROLE_PREFIX)
}

/// The listing-package string for a document addressed by more than a package
/// name: `{package}@{address}`.
///
/// Two documents need one. A collection's **version document** is one version
/// of one collection, and a **role versions** listing is addressed upstream by
/// a numeric role id that nothing else in this instance speaks. Both are cached
/// per `(package, document kind)`, so the extra coordinate has to travel in the
/// package string — the arrangement `sdkman` uses for its platform and `rustup`
/// for its channel.
///
/// `@` is the separator because no galaxy name, version or role id can contain
/// one, so [`package_of`] is unambiguous — and it is what
/// `RegistryKind::blocking_package_name` strips, so a block written on
/// `community.general` is found from a request for one of its versions.
pub fn addressed(package: &str, address: &str) -> String {
    format!("{package}@{address}")
}

/// The package half of an [`addressed`] listing string, or the string itself.
///
/// What a block is written on: an operator blocking `community.general` means
/// the collection, not the one version whose document happened to be requested.
pub fn package_of(listing: &str) -> &str {
    listing.split('@').next().unwrap_or(listing)
}

/// The address half of an [`addressed`] listing string — a version for a
/// collection's version document, a numeric role id for a role listing.
pub fn address_of(listing: &str) -> Option<&str> {
    listing.split_once('@').map(|(_, address)| address)
}

/// The tarball filename for a collection version, the one spelling §4.3
/// requires.
pub fn artifact_filename(namespace: &str, name: &str, version: &str) -> String {
    format!("{namespace}-{name}-{version}.tar.gz")
}

/// Parse `{ns}-{name}-{version}.tar.gz` back into its coordinate.
///
/// The inverse of [`artifact_filename`], and the check the artifact handler
/// applies before the filename becomes a storage key: a name and a version both
/// contain no `-` by their own rules (`[a-z][a-z0-9_]*` and semver), so the
/// split is unambiguous — the first two `-`-separated fields are the halves of
/// the collection and everything after them is the version.
pub fn parse_artifact_filename(filename: &str) -> Result<(String, String), CoreError> {
    let stem = filename.strip_suffix(".tar.gz").ok_or_else(|| {
        CoreError::InvalidInput(format!("'{filename}' is not a collection artifact name"))
    })?;
    let mut parts = stem.splitn(3, '-');
    let (Some(namespace), Some(name), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(CoreError::InvalidInput(format!(
            "'{filename}' is not '{{namespace}}-{{name}}-{{version}}.tar.gz'"
        )));
    };
    let package = collection_package(namespace, name)?;
    validate_version(version)?;
    Ok((package, version.to_owned()))
}

/// A collection version, as far as anything that becomes a key is concerned.
///
/// Galaxy requires semver, and `ansible-galaxy` parses every version it reads
/// as one. This is deliberately narrower than a full semver parse: the property
/// that matters here is that no separator, traversal segment or control
/// character reaches a storage key.
pub fn validate_version(version: &str) -> Result<(), CoreError> {
    if version.is_empty()
        || version.len() > 128
        || version.contains("..")
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
    {
        return Err(CoreError::InvalidInput(format!(
            "'{version}' is not a collection version"
        )));
    }
    Ok(())
}

fn is_galaxy_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && s.len() <= 64
}

// --- MANIFEST.json --------------------------------------------------------

/// `MANIFEST.json`'s `collection_info` — everything a publish reads out of a
/// tarball, and nothing this instance re-derives.
///
/// `FILES.json` travels inside the artifact and is never rebuilt: it is what a
/// client verifies the extracted tree against, so a re-derived copy that
/// disagreed by one byte would fail an install this instance had accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionInfo {
    pub namespace: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub dependencies: Map<String, Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub license: Vec<String>,
    #[serde(default)]
    pub readme: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub documentation: Option<String>,
    #[serde(default)]
    pub issues: Option<String>,
}

/// The `MANIFEST.json` at the root of every collection tarball.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub collection_info: CollectionInfo,
    #[serde(default)]
    pub file_manifest_file: Option<Value>,
    #[serde(default)]
    pub format: Option<u32>,
}

impl Manifest {
    /// The dotted package name this manifest claims.
    pub fn package(&self) -> Result<String, CoreError> {
        collection_package(&self.collection_info.namespace, &self.collection_info.name)
    }
}

// --- the documents this instance composes ---------------------------------

/// The absolute path of a collection's artifact under `base`.
///
/// Absolute, because `_download_file` refuses a `download_url` that is neither
/// absolute nor absolute-path (*"Invalid non absolute download_url"*), and
/// because `urljoin` against whatever api_server the operator configured has to
/// land back on this registry under both routings.
pub fn artifact_url(base: &str, namespace: &str, name: &str, version: &str) -> String {
    let file = artifact_filename(namespace, name, version);
    format!("{}/galaxy/api/v3/artifacts/collections/{file}", trim(base))
}

/// The absolute path of a collection's version document.
pub fn version_url(base: &str, namespace: &str, name: &str, version: &str) -> String {
    format!(
        "{}/galaxy/api/v3/collections/{namespace}/{name}/versions/{version}/",
        trim(base)
    )
}

/// The absolute path of a collection document.
pub fn collection_url(base: &str, namespace: &str, name: &str) -> String {
    format!(
        "{}/galaxy/api/v3/collections/{namespace}/{name}/",
        trim(base)
    )
}

/// The absolute path of a collection's versions listing.
pub fn versions_url(base: &str, namespace: &str, name: &str) -> String {
    format!(
        "{}/galaxy/api/v3/collections/{namespace}/{name}/versions/",
        trim(base)
    )
}

fn trim(base: &str) -> &str {
    base.trim_end_matches('/')
}

/// One entry of a composed versions list.
#[derive(Debug, Clone)]
pub struct VersionEntry {
    pub version: String,
    /// RFC 3339. `None` for a locally published version whose row carries no
    /// timestamp, which is exactly the case §4.5 makes an age gate decide
    /// about explicitly.
    pub created_at: Option<String>,
    pub requires_ansible: Option<String>,
}

/// Render a versions listing in the shape §4.4 fixes: `data`, `meta.count`, and
/// every `links` member null.
///
/// `data` rather than `results` in both modes. galaxy_ng answers with `data`,
/// a standalone pulp_ansible with `results`, and the client accepts either
/// (`for key in ['data', 'results']`) — so one shape is one set of fixtures.
pub fn compose_versions(
    base: &str,
    namespace: &str,
    name: &str,
    entries: &[VersionEntry],
) -> Value {
    let data: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut obj = Map::new();
            obj.insert("version".to_owned(), json!(entry.version));
            obj.insert(
                "href".to_owned(),
                json!(version_url(base, namespace, name, &entry.version)),
            );
            if let Some(created) = &entry.created_at {
                obj.insert("created_at".to_owned(), json!(created));
                obj.insert("updated_at".to_owned(), json!(created));
            }
            obj.insert(
                "requires_ansible".to_owned(),
                entry
                    .requires_ansible
                    .as_ref()
                    .map(|r| json!(r))
                    .unwrap_or(Value::Null),
            );
            obj.insert("marks".to_owned(), json!([]));
            Value::Object(obj)
        })
        .collect();
    json!({
        "meta": { "count": data.len() },
        "links": one_page(),
        "data": data,
    })
}

/// The collection document, composed for a local or hybrid registry.
///
/// `updated_at` is the caller's: for a local collection it is the newest
/// publish, and a block bumps it past that (§4.4), because
/// `get_collection_versions` drops its cached copy of the versions list when
/// this field moves.
pub fn compose_collection(
    base: &str,
    namespace: &str,
    name: &str,
    highest: Option<&str>,
    updated_at: Option<&str>,
) -> Value {
    json!({
        "href": collection_url(base, namespace, name),
        "namespace": namespace,
        "name": name,
        "deprecated": false,
        "versions_url": versions_url(base, namespace, name),
        "highest_version": highest.map(|v| json!({
            "href": version_url(base, namespace, name, v),
            "version": v,
        })).unwrap_or(Value::Null),
        "created_at": updated_at,
        "updated_at": updated_at,
    })
}

/// Every `links` member null — the one-page invariant, in the shape the v3
/// documents use.
pub fn one_page() -> Value {
    json!({ "first": null, "previous": null, "next": null, "last": null })
}

/// Collapse an assembled v3 listing to one page, in place.
///
/// Called by the adapter once it has walked upstream's pages, and by the local
/// composer for symmetry. Separate from the block filter because it happens
/// whether or not anything is blocked: a relayed `links.next` is an absolute
/// path that `urljoin` resolves against the *root* of the host, which on a
/// path-routed instance is a `404` and on a host-routed one is another
/// registry's namespace.
pub fn collapse_to_one_page(doc: &mut Value) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    let count = obj
        .get("data")
        .or_else(|| obj.get("results"))
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    obj.insert("links".to_owned(), one_page());
    let meta = obj
        .entry("meta")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(meta) = meta {
        meta.insert("count".to_owned(), json!(count));
    }
}

/// The discovery document this instance serves.
///
/// Composed here rather than relayed: it advertises *this* instance's API
/// versions, and `roles = "off"` has to remove `v1` from it as well as from the
/// routes. `ansible-galaxy` refuses an action whose API version is absent with
/// its own *"Galaxy action … requires API versions 'v1'"*, which is a better
/// failure than a `404` an operator reads as a proxy fault.
pub fn discovery(serve_roles: bool) -> Value {
    let mut versions = Map::new();
    if serve_roles {
        versions.insert("v1".to_owned(), json!("v1/"));
    }
    versions.insert("v3".to_owned(), json!("v3/"));
    json!({
        "description": "BatleHub Ansible Galaxy proxy",
        "available_versions": Value::Object(versions),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collection_name_is_two_lower_case_identifiers() {
        assert_eq!(
            parse_collection("community.general").unwrap(),
            ("community", "general")
        );
        assert_eq!(parse_collection("acme.util_2").unwrap(), ("acme", "util_2"));
    }

    #[test]
    fn a_traversal_is_not_a_collection_name() {
        for bad in [
            "../../etc/passwd",
            "community.general.extra",
            "community",
            "Community.General",
            "community.",
            ".general",
            "community.gen-eral",
            "community/general",
            "9community.general",
        ] {
            assert!(
                parse_collection(bad).is_err(),
                "{bad} should not parse as a collection name"
            );
        }
    }

    #[test]
    fn the_artifact_filename_round_trips() {
        let file = artifact_filename("community", "general", "13.4.0");
        assert_eq!(file, "community-general-13.4.0.tar.gz");
        assert_eq!(
            parse_artifact_filename(&file).unwrap(),
            ("community.general".to_owned(), "13.4.0".to_owned())
        );
    }

    #[test]
    fn a_prerelease_version_survives_the_round_trip() {
        // The version keeps every `-` after the first two fields, which is what
        // `splitn(3, '-')` is for.
        let file = artifact_filename("acme", "util", "1.0.0-rc.1");
        assert_eq!(
            parse_artifact_filename(&file).unwrap(),
            ("acme.util".to_owned(), "1.0.0-rc.1".to_owned())
        );
    }

    #[test]
    fn an_artifact_filename_that_walks_is_refused() {
        for bad in [
            "../../etc/passwd.tar.gz",
            "community-general-../1.0.0.tar.gz",
            "community-general.tar.gz",
            "community-general-1.0.0.zip",
        ] {
            assert!(
                parse_artifact_filename(bad).is_err(),
                "{bad} should not parse as an artifact filename"
            );
        }
    }

    #[test]
    fn a_role_key_carries_its_prefix_and_refuses_a_separator() {
        assert_eq!(
            role_key("geerlingguy", "docker").unwrap(),
            "roles/geerlingguy.docker"
        );
        assert!(is_role("roles/geerlingguy.docker"));
        assert!(!is_role("community.general"));
        assert!(role_key("..", "docker").is_err());
        assert!(role_key("geerlingguy", "doc/ker").is_err());
    }

    #[test]
    fn a_composed_listing_is_one_page() {
        let entries = [
            VersionEntry {
                version: "1.0.0".to_owned(),
                created_at: Some("2026-01-01T00:00:00Z".to_owned()),
                requires_ansible: Some(">=2.15.0".to_owned()),
            },
            VersionEntry {
                version: "1.1.0".to_owned(),
                created_at: None,
                requires_ansible: None,
            },
        ];
        let doc = compose_versions("https://h/proxy/g", "acme", "util", &entries);
        assert_eq!(doc["meta"]["count"], 2);
        for link in ["first", "previous", "next", "last"] {
            assert!(doc["links"][link].is_null(), "{link} must be null");
        }
        assert_eq!(doc["data"][0]["version"], "1.0.0");
        assert_eq!(
            doc["data"][0]["href"],
            "https://h/proxy/g/galaxy/api/v3/collections/acme/util/versions/1.0.0/"
        );
        // An entry with no timestamp says so by omission rather than by a
        // guessed date.
        assert!(doc["data"][1].get("created_at").is_none());
    }

    #[test]
    fn collapsing_a_relayed_page_nulls_its_links_and_recounts() {
        let mut doc = serde_json::json!({
            "meta": { "count": 241 },
            "links": { "next": "/api/v3/plugin/x?offset=100", "first": "/api/v3/x" },
            "data": [ {"version": "1.0.0"}, {"version": "1.1.0"} ],
        });
        collapse_to_one_page(&mut doc);
        assert_eq!(doc["meta"]["count"], 2);
        assert!(doc["links"]["next"].is_null());
        assert!(doc["links"]["first"].is_null());
    }

    #[test]
    fn discovery_drops_v1_when_roles_are_off() {
        let on = discovery(true);
        assert_eq!(on["available_versions"]["v1"], "v1/");
        assert_eq!(on["available_versions"]["v3"], "v3/");
        let off = discovery(false);
        assert!(off["available_versions"].get("v1").is_none());
        assert_eq!(off["available_versions"]["v3"], "v3/");
    }

    #[test]
    fn the_artifact_url_ends_in_the_upstream_filename() {
        // `_download_file` slices `.tar.gz` off the last path segment to name
        // the file it writes, so the served path has to end in that name.
        assert_eq!(
            artifact_url("https://h/proxy/g/", "community", "general", "13.4.0"),
            "https://h/proxy/g/galaxy/api/v3/artifacts/collections/community-general-13.4.0.tar.gz"
        );
    }
}
