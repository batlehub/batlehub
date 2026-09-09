use chrono::{DateTime, Utc};

use super::{artifact_storage_key, CoreError, Identity, LocalRegistryService, PublishedPackage};
use crate::entities::Action;
use crate::ports::{collect_byte_stream, StorageMeta};

/// One published VS Code extension version, decoded from the per-version
/// `index_metadata` written at publish time.
///
/// The web layer renders both client protocols from this — the VS Code gallery
/// (`extensionquery`) and the OpenVSX REST API — so it carries everything
/// either of them shows, not just what an install needs. An editor that gets a
/// bare id and version renders a blank tile with no description, no categories
/// and no engine constraint, which is a working install and a useless
/// marketplace.
#[derive(Debug, Clone)]
pub struct OpenVsxExtensionVersion {
    /// `{publisher}.{name}`, the id the whole ecosystem addresses by.
    pub extension_id: String,
    /// The `{publisher}` half, split out because both protocols report it
    /// separately.
    pub publisher: String,
    /// The `{name}` half.
    pub name: String,
    pub version: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// The `engines.vscode` range — the editor refuses to install a version
    /// whose engine it does not satisfy, so an absent value is not the same as
    /// a permissive one and stays `None` rather than becoming `*`.
    pub engine: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub extension_pack: Vec<String>,
    pub dependencies: Vec<String>,
    /// Path of the icon *inside* the VSIX, when the manifest declares one.
    pub icon_path: Option<String>,
    /// A pre-release in the VS Code sense (`--pre-release` at package time),
    /// which the editor hides unless the user opted in.
    pub pre_release: bool,
    /// SHA-256 hex of the VSIX bytes.
    pub checksum: String,
    pub published_at: DateTime<Utc>,
    /// RFC 0020 §13.6: the version's signature archive was provided at
    /// publish time (an upstream's, served as-is) rather than made by the
    /// registry's key.
    pub signature_provided: bool,
}

impl OpenVsxExtensionVersion {
    fn from_published(pkg: &PublishedPackage) -> Self {
        let m = &pkg.index_metadata;
        let s = |key: &str| {
            m.get(key)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let list = |key: &str| {
            m.get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .filter(|x| !x.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };

        // `publisher` is recorded at publish time, but an extension published
        // before that field existed still has a well-formed id to split.
        let publisher = s("publisher")
            .or_else(|| pkg.name.split('.').next().map(str::to_owned))
            .unwrap_or_default();
        let name = pkg
            .name
            .split_once('.')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_else(|| pkg.name.clone());

        Self {
            extension_id: pkg.name.clone(),
            publisher,
            name,
            version: pkg.version.clone(),
            display_name: s("displayName"),
            description: s("description"),
            engine: s("engine"),
            categories: list("categories"),
            tags: list("keywords"),
            extension_pack: list("extensionPack"),
            dependencies: list("extensionDependencies"),
            icon_path: s("icon"),
            pre_release: m
                .get("preRelease")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            checksum: pkg.checksum.clone(),
            published_at: pkg.published_at,
            signature_provided: m.get("vsixSignature").and_then(|v| v.as_str()) == Some("provided"),
        }
    }

    /// Whether `query` matches this extension, for the free-text search both
    /// protocols expose.
    ///
    /// Substring, case-insensitive, over the fields a user would type: the id,
    /// the display name, the description and the keywords. Deliberately not a
    /// ranked search — a local registry holds tens of extensions, not tens of
    /// thousands, and a relevance model nobody can explain is worse than an
    /// obvious one.
    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let q = query.to_lowercase();
        let hit = |s: &str| s.to_lowercase().contains(&q);
        hit(&self.extension_id)
            || self.display_name.as_deref().is_some_and(hit)
            || self.description.as_deref().is_some_and(hit)
            || self.tags.iter().any(|t| hit(t))
    }
}

/// The sibling key a **provided** signature archive is kept under (RFC 0020
/// §13.6) — distinct from the registry-signed `.sigzip`, so the
/// verify-or-rebuild of the latter never touches it.
pub fn provided_vsix_signature_key(registry: &str, name: &str, version: &str) -> String {
    format!(
        "{}.sigzip.upstream",
        artifact_storage_key(registry, name, version)
    )
}

impl LocalRegistryService {
    /// Attach an upstream's signature archive to a version this registry
    /// holds (RFC 0020 §13.6): a marketplace extension republished locally
    /// keeps the signature a stock editor verifies. The archive must be a
    /// zip holding `.signature.manifest` — checked against the stored VSIX
    /// bytes, so an archive made over other bytes is refused — and at least
    /// one signature entry. The registry vouches for nothing in it and
    /// never rewrites it. Authorised as a publish of the same version.
    pub async fn attach_vsix_signature(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        archive: bytes::Bytes,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesPublish)
            .await?;
        let versions = self.backend.get_versions(registry, name).await?;
        if !versions.iter().any(|p| p.version == version) {
            return Err(CoreError::NotFound(format!(
                "{name}@{version} is not published in registry '{registry}'"
            )));
        }
        let vsix_key = artifact_storage_key(registry, name, version);
        let stored = self.storage.retrieve(&vsix_key).await?.ok_or_else(|| {
            CoreError::NotFound(format!("{name}@{version}: no stored VSIX to sign for"))
        })?;
        let vsix = collect_byte_stream(stored.stream).await?;

        let provided = crate::services::vsx_signature::read_provided_archive(&archive)?;
        if !crate::services::vsx_signature::manifest_matches(&provided.manifest, &vsix)? {
            return Err(CoreError::InvalidInput(format!(
                "the signature manifest does not describe the stored {name}@{version}: \
                 the archive was made over other bytes"
            )));
        }

        let key = provided_vsix_signature_key(registry, name, version);
        self.storage
            .store(
                &key,
                archive.clone(),
                StorageMeta {
                    content_type: Some("application/zip".to_owned()),
                    size: Some(archive.len() as u64),
                    checksum: None,
                },
            )
            .await?;
        self.backend
            .set_vsix_signature_provided(registry, name, version, true)
            .await?;
        tracing::info!(
            registry,
            name,
            version,
            entries = ?provided.entries,
            "attached a provided VSIX signature archive"
        );
        Ok(())
    }

    /// The provided archive for a version, when one was attached.
    pub async fn provided_vsix_signature(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<bytes::Bytes>, CoreError> {
        let key = provided_vsix_signature_key(registry, name, version);
        match self.storage.retrieve(&key).await? {
            Some(stored) => Ok(Some(collect_byte_stream(stored.stream).await?)),
            None => Ok(None),
        }
    }

    /// All visible, non-yanked versions of one extension, oldest first.
    ///
    /// Goes through `load_visible_versions_or_not_found`, the chokepoint that
    /// already applies `filter_unlisted → filter_blocked → filter_for_identity`
    /// — so an administratively blocked version is absent from every document
    /// the gallery renders without the gallery having to know about blocking.
    ///
    /// Yanked versions are dropped here: neither the VS Code gallery nor the
    /// OpenVSX API has a representation for "exists but withdrawn", so listing
    /// one would offer the editor a version it will then refuse to serve.
    pub async fn get_openvsx_versions(
        &self,
        registry: &str,
        extension_id: &str,
        identity: &Identity,
    ) -> Result<Vec<OpenVsxExtensionVersion>, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, extension_id, identity, "extension")
            .await?;
        let versions: Vec<_> = versions
            .iter()
            .filter(|p| !p.yanked)
            .map(OpenVsxExtensionVersion::from_published)
            .collect();
        if versions.is_empty() {
            return Err(CoreError::NotFound(format!(
                "extension '{extension_id}' not found in local registry '{registry}'"
            )));
        }
        Ok(versions)
    }

    /// Every extension in `registry` matching `query`, as one group of visible
    /// versions per extension — for the search both protocols expose.
    ///
    /// **All the versions, not just the newest.** A client that looks an
    /// extension up by its *uuid* (`extensionquery` filter type 4) is answered
    /// from here rather than from the by-id fast path, and VS Code takes exactly
    /// that route when the newest version is a pre-release and it has to fall
    /// back to the last release. Answered with a single version, the editor
    /// concludes the extension "has no release version" and refuses to install
    /// it — measured against a real `code --install-extension`.
    ///
    /// `query` is matched against the **newest** version's fields, since that is
    /// the one a search result renders.
    ///
    /// Packages whose visibility check denies `identity` are skipped rather
    /// than surfaced as an error: a search should show what the caller may see,
    /// and a `403` from one private extension must not blank the whole result
    /// set. Groups are sorted by extension id so paging is stable across
    /// requests.
    pub async fn get_openvsx_extensions(
        &self,
        registry: &str,
        query: &str,
        identity: &Identity,
    ) -> Result<Vec<Vec<OpenVsxExtensionVersion>>, CoreError> {
        let names = self.backend.list_package_names(registry).await?;
        let (readable, grant_rows) = self.readable_packages(registry, identity).await?;
        let mut hits: Vec<Vec<OpenVsxExtensionVersion>> = Vec::new();
        for name in names {
            // RFC 0015 §4.4, and finding 11's lesson one layer up: a search is a
            // listing, and a listing shows what this caller may see.
            if !readable.contains(&name) {
                continue;
            }
            let versions = match self
                .load_visible_versions_in(registry, &name, identity, Some(&grant_rows))
                .await
            {
                Ok(v) => v,
                Err(CoreError::AccessDenied(_)) => continue,
                Err(e) => return Err(e),
            };
            // Yanked versions are dropped for the same reason
            // `get_openvsx_versions` drops them: neither protocol can say
            // "exists but withdrawn".
            let visible: Vec<OpenVsxExtensionVersion> = versions
                .iter()
                .filter(|p| !p.yanked)
                .map(OpenVsxExtensionVersion::from_published)
                .collect();

            // Newest by publish date, not by version string: extension versions
            // are conventionally semver but nothing enforces it, and `1.10.0`
            // sorts below `1.9.0` lexicographically. Same reasoning as
            // `newest_jetbrains_match`.
            let Some(newest) = visible.iter().max_by_key(|v| v.published_at) else {
                continue;
            };
            if newest.matches(query) {
                hits.push(visible);
            }
        }
        hits.sort_by(|a, b| group_id(a).cmp(group_id(b)));
        Ok(hits)
    }
}

/// The extension id a group of versions belongs to. Every group this module
/// builds is non-empty; an empty one sorts first rather than panicking.
fn group_id(group: &[OpenVsxExtensionVersion]) -> &str {
    group
        .first()
        .map(|v| v.extension_id.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Visibility;

    fn published(name: &str, version: &str, meta: serde_json::Value) -> PublishedPackage {
        PublishedPackage {
            registry: "vsx".to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            checksum: "abc".to_owned(),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: meta,
            published_at: Utc::now(),
            published_by: None,
            signature_bytes: None,
            signature_type: None,
            visibility: Visibility::Public,
            retention_keep: false,
        }
    }

    #[test]
    fn the_extension_id_splits_into_publisher_and_name() {
        let v = OpenVsxExtensionVersion::from_published(&published(
            "ms-python.python",
            "1.0.0",
            serde_json::json!({}),
        ));
        assert_eq!(v.publisher, "ms-python");
        assert_eq!(v.name, "python");
        assert_eq!(v.extension_id, "ms-python.python");
    }

    /// An extension name may itself contain dots; only the first separates the
    /// publisher, matching the adapters' `parse_id`.
    #[test]
    fn only_the_first_dot_separates_the_publisher() {
        let v = OpenVsxExtensionVersion::from_published(&published(
            "acme.my.extension",
            "1.0.0",
            serde_json::json!({}),
        ));
        assert_eq!(v.publisher, "acme");
        assert_eq!(v.name, "my.extension");
    }

    #[test]
    fn manifest_fields_are_decoded() {
        let v = OpenVsxExtensionVersion::from_published(&published(
            "acme.tool",
            "2.0.0",
            serde_json::json!({
                "displayName": "Acme Tool",
                "description": "Does the thing",
                "engine": "^1.85.0",
                "categories": ["Linters", "Other"],
                "keywords": ["acme", "lint"],
                "extensionPack": ["acme.other"],
                "extensionDependencies": ["ms-python.python"],
                "icon": "images/icon.png",
                "preRelease": true
            }),
        ));
        assert_eq!(v.display_name.as_deref(), Some("Acme Tool"));
        assert_eq!(v.engine.as_deref(), Some("^1.85.0"));
        assert_eq!(v.categories, ["Linters", "Other"]);
        assert_eq!(v.tags, ["acme", "lint"]);
        assert_eq!(v.extension_pack, ["acme.other"]);
        assert_eq!(v.dependencies, ["ms-python.python"]);
        assert_eq!(v.icon_path.as_deref(), Some("images/icon.png"));
        assert!(v.pre_release);
    }

    /// An extension published before the richer metadata existed must still
    /// decode — it renders plainly rather than failing the whole listing.
    #[test]
    fn a_bare_index_metadata_still_decodes() {
        let v = OpenVsxExtensionVersion::from_published(&published(
            "acme.tool",
            "1.0.0",
            serde_json::json!({ "id": "acme.tool", "version": "1.0.0" }),
        ));
        assert_eq!(v.publisher, "acme");
        assert!(v.display_name.is_none());
        assert!(v.categories.is_empty());
        assert!(!v.pre_release);
    }

    /// Empty strings in the manifest are absent values, not empty labels.
    #[test]
    fn empty_strings_are_treated_as_absent() {
        let v = OpenVsxExtensionVersion::from_published(&published(
            "acme.tool",
            "1.0.0",
            serde_json::json!({ "displayName": "", "description": "", "categories": ["", "Other"] }),
        ));
        assert!(v.display_name.is_none());
        assert!(v.description.is_none());
        assert_eq!(v.categories, ["Other"]);
    }

    #[test]
    fn search_matches_id_display_name_description_and_keywords() {
        let v = OpenVsxExtensionVersion::from_published(&published(
            "acme.tool",
            "1.0.0",
            serde_json::json!({
                "displayName": "Acme Tool",
                "description": "Formats YAML",
                "keywords": ["linting"]
            }),
        ));
        assert!(v.matches("acme"), "id");
        assert!(v.matches("TOOL"), "case-insensitive");
        assert!(v.matches("yaml"), "description");
        assert!(v.matches("lint"), "keyword substring");
        assert!(v.matches(""), "an empty query matches everything");
        assert!(!v.matches("python"));
    }
}
