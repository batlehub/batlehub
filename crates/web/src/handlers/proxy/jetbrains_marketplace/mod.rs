//! JetBrains Marketplace (plugins.jetbrains.com) handlers.
//!
//! Emulates the IDE-facing marketplace surface (search, compatible-updates,
//! meta.json blobs, downloads) plus the `updatePlugins.xml` custom-repo XML and
//! a marketplace-compatible multipart publish, across local/hybrid/proxy modes.
//!
//! Offline resilience: per-plugin endpoints render from the ProxyService
//! metadata cache (`resolve_metadata_for`, stale-capable), artifacts go through
//! the ProxyService storage cache, and query/list endpoints use the
//! [`cached_forward`] body cache — anything seen once keeps working after
//! upstream loss.

pub mod cached_forward;
mod files;
mod ide;
mod plugin_archive;
mod publish;
mod render;
mod xml;

pub use files::{
    jbm_broken_plugins, jbm_file_download, jbm_ide_extensions, jbm_jb_plugins_xml_ids,
    jbm_plugin_download, jbm_plugin_manager, jbm_plugin_meta, jbm_plugins_xml_ids, jbm_update_meta,
};
pub use ide::{
    jbm_aggregation, jbm_comments, jbm_compatible_updates, jbm_compatible_updates_get,
    jbm_feature_implementations, jbm_plugin_info, jbm_plugin_updates, jbm_search_plugins,
    jbm_search_plugins_ide,
};
pub use publish::jbm_upload;
pub use xml::{jbm_plugins_list, jbm_update_plugins_xml};

use super::common::registry_public_base;
use crate::{error::AppError, RegistryMap};

pub(crate) const REGISTRY_TYPE: &str = "jetbrains-marketplace";

/// The Stable channel is the empty string, matching marketplace conventions.
pub(crate) const STABLE_CHANNEL: &str = "";

/// `PackageId::artifact` sub-coordinate the plugin archive is cached under, so
/// the download endpoints and cache warming share one storage slot.
///
/// Must stay equal to `RegistryKind::JetbrainsMarketplace.warm_artifact()` —
/// warming pre-fetches into exactly this key, and the test below fails if the
/// two ever drift. A non-Stable channel appends `@{channel}`, which warming
/// does not pre-fetch (`/plugins/list` only enumerates Stable).
pub(crate) const PLUGIN_ARTIFACT: &str = "plugin";

pub(crate) fn require_jbm(registry: &str, map: &RegistryMap) -> Result<(), AppError> {
    super::common::require_registry_type(registry, REGISTRY_TYPE, map)
}

/// Reject path parameters that would span segments when interpolated into a
/// forwarded upstream path: actix percent-decodes `%2F` before extraction, and
/// `validate_package_name` deliberately admits interior `/`.
pub(crate) fn require_single_segment(kind: &str, value: &str) -> Result<(), AppError> {
    if value.contains('/') {
        return Err(AppError::bad_request(format!(
            "{kind} must be a single path segment"
        )));
    }
    Ok(())
}

pub(crate) fn content_type_for(file_name: &str) -> &'static str {
    if file_name.ends_with(".zip") {
        "application/zip"
    } else if file_name.ends_with(".jar") {
        "application/java-archive"
    } else if file_name.ends_with(".xml") {
        "application/xml"
    } else if file_name.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::{content_type_for, PLUGIN_ARTIFACT};
    use batlehub_core::entities::{FetchArtifact, RegistryKind};

    /// Warming writes the plugin archive under
    /// `RegistryKind::warm_artifact()`; the download handlers read it back under
    /// `PLUGIN_ARTIFACT`. If these diverge, warmed plugins silently stop being
    /// served from cache.
    #[test]
    fn plugin_artifact_matches_the_warming_coordinate() {
        assert_eq!(
            RegistryKind::JetbrainsMarketplace.warm_artifact(),
            Some(FetchArtifact::Fixed(PLUGIN_ARTIFACT))
        );
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type_for("p.zip"), "application/zip");
        assert_eq!(content_type_for("p.jar"), "application/java-archive");
        assert_eq!(content_type_for("updatePlugins.xml"), "application/xml");
        assert_eq!(content_type_for("meta.json"), "application/json");
        assert_eq!(content_type_for("blob"), "application/octet-stream");
    }
}
