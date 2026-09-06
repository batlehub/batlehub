use serde::{Deserialize, Serialize};

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct ExtensionQueryRequest {
    pub filters: Vec<ExtensionQueryFilter>,
    pub flags: u32,
}

#[derive(Serialize)]
pub(super) struct ExtensionQueryFilter {
    pub criteria: Vec<ExtensionQueryCriteria>,
}

#[derive(Serialize)]
pub(super) struct ExtensionQueryCriteria {
    #[serde(rename = "filterType")]
    pub filter_type: u32,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ExtensionQueryResponse {
    pub results: Vec<ExtensionQueryResult>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ExtensionQueryResult {
    pub extensions: Vec<VSCodeExtension>,
}

#[derive(Debug, Deserialize)]
pub(super) struct VSCodeExtension {
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "shortDescription")]
    pub description: Option<String>,
    pub versions: Vec<VSCodeExtensionVersion>,
}

#[derive(Debug, Deserialize)]
pub(super) struct VSCodeExtensionVersion {
    pub version: String,
    #[serde(rename = "lastUpdated")]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub files: Vec<VSCodeExtensionFile>,
}

#[derive(Debug, Deserialize)]
pub(super) struct VSCodeExtensionFile {
    #[serde(rename = "assetType")]
    pub asset_type: String,
    pub source: String,
}

pub(super) struct ResolvedExtension {
    pub version_info: VSCodeExtensionVersion,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

// ── API constants ─────────────────────────────────────────────────────────────

pub(super) const VSIX_ASSET_TYPE: &str = "Microsoft.VisualStudio.Services.VSIXPackage";
/// The signature archive beside the package (RFC 0020 §4.2), relayed as-is.
pub(super) const SIGNATURE_ASSET_TYPE: &str = "Microsoft.VisualStudio.Services.VsixSignature";
/// The extension's README, served as a separate asset URL rather than inline.
///
/// Following it is an outbound request in its own right, so it travels as a
/// link and is read in the detached introspection task, same-origin checked
/// against this registry's own base URL (RFC 0007 §7.4).
pub(super) const README_ASSET_TYPE: &str = "Microsoft.VisualStudio.Services.Content.Details";
pub(super) const GALLERY_API_ACCEPT: &str = "application/json;api-version=3.0-preview.1";

// filterType values for extensionquery criteria
pub(super) const FILTER_EXTENSION_NAME: u32 = 7;
pub(super) const FILTER_VERSION: u32 = 10;

// flags bitmask for extensionquery
pub(super) const FLAG_INCLUDE_VERSIONS: u32 = 0x001;
pub(super) const FLAG_INCLUDE_FILES: u32 = 0x002;
pub(super) const FLAG_INCLUDE_ASSET_URI: u32 = 0x080;
pub(super) const FLAG_INCLUDE_LATEST_ONLY: u32 = 0x200;
