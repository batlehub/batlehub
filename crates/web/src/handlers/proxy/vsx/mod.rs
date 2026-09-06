//! VS Code extension gallery and OpenVSX API.
//!
//! One module serving **two client protocols** for **two registry kinds**:
//!
//! - the VS Code gallery (`/vscode/gallery/extensionquery` and friends), which
//!   is what `extensionsGallery.serviceUrl` in an editor's `product.json`
//!   points at;
//! - the OpenVSX native REST API (`/api/…`), which `ovsx` and openvsx-native
//!   clients speak.
//!
//! Both are the *client* side, so both work for `openvsx` and for
//! `vscode-marketplace` — the kinds differ only in which upstream they proxy.
//! Before this existed, the entire client surface was the raw VSIX download, so
//! BatleHub could cache extension bytes but could not be an editor's
//! marketplace.
//!
//! # Shape
//!
//! Following `jetbrains_marketplace/`, which solved the same problem one
//! protocol earlier: [`render`] is pure and holds the intermediate
//! [`render::GalleryEntry`] plus every document builder; [`source`] is the one
//! place entries are produced from local or upstream data **and the one place
//! administratively blocked versions are removed**; the handler files render
//! and nothing else.
//!
//! # An editor cannot authenticate
//!
//! This is the operational fact that decides whether a deployment works. VS
//! Code sends **no `Authorization` header** to its gallery, and `product.json`
//! has nowhere to put one. A registry used as a gallery therefore needs
//! `anonymous` read in its `[registries.rbac]`, or an ingress that
//! authenticates in front of BatleHub. A gallery registry that requires a
//! bearer token answers every query with an empty list and the editor reports
//! no extensions found — which looks like a broken proxy rather than a
//! configuration choice.

use batlehub_core::entities::RegistryKind;

use crate::{error::AppError, RegistryMap};

pub mod api;
pub mod archive;
pub mod assets;
pub mod gallery;
pub mod protocol;
pub mod render;
pub mod signing;
pub mod source;

pub use api::{
    openvsx_extension, openvsx_extension_version, openvsx_file, openvsx_namespace,
    openvsx_namespace_create, openvsx_public_key, openvsx_publish, openvsx_search, openvsx_version,
};
pub use assets::{vsx_asset, vsx_item, vsx_unpkg, vsx_vspackage};
pub use gallery::vsx_extension_query;

/// The `PackageId::artifact` sub-coordinate a VSIX is stored and read under.
///
/// Shared with `handlers/proxy/openvsx.rs`, whose drift-guard test ties it to
/// `RegistryKind::warm_artifact()`.
pub(crate) const VSIX_ARTIFACT: &str = "vsix";

/// Accept both extension registry kinds.
///
/// The gallery is a client protocol; which upstream sits behind it is the
/// registry's business, not the editor's.
pub(crate) fn require_vsx(registry: &str, map: &RegistryMap) -> Result<(), AppError> {
    super::openvsx::require_openvsx(registry, map)
}

/// Which of the two kinds this registry is, for `blocked_versions_for`.
pub(crate) fn vsx_kind(registry: &str, map: &RegistryMap) -> RegistryKind {
    match map.type_of(registry).as_deref() {
        Some("vscode-marketplace") => RegistryKind::VscodeMarketplace,
        // `require_vsx` has already rejected anything that is neither kind.
        _ => RegistryKind::Openvsx,
    }
}

/// Reject a path segment that is not a single segment.
///
/// actix percent-decodes before extraction, so a `%2F` in a publisher or
/// extension name arrives as a real `/` and would silently widen the
/// coordinate. Same guard `jetbrains_marketplace` applies for the same reason.
pub(crate) fn require_single_segment(kind: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.contains('/') || value.contains('\\') {
        return Err(AppError::bad_request(format!("invalid {kind}: '{value}'")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vsix_artifact_slot_is_shared_with_the_download_route() {
        assert_eq!(VSIX_ARTIFACT, "vsix");
        for kind in [RegistryKind::Openvsx, RegistryKind::VscodeMarketplace] {
            assert_eq!(
                kind.warm_artifact(),
                Some(batlehub_core::entities::FetchArtifact::Fixed(VSIX_ARTIFACT))
            );
        }
    }

    #[test]
    fn a_decoded_slash_is_rejected_rather_than_widening_the_coordinate() {
        assert!(require_single_segment("publisher", "acme").is_ok());
        assert!(require_single_segment("publisher", "acme/evil").is_err());
        assert!(require_single_segment("publisher", "acme\\evil").is_err());
        assert!(require_single_segment("publisher", "").is_err());
    }
}
