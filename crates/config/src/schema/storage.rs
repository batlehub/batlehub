use serde::Deserialize;

// `S3StorageConfig` lives in `batlehub_core::ports` so the S3 storage adapter
// in `adapters` doesn't need a dependency on this crate. Re-exported here so
// existing `batlehub_config::schema::S3StorageConfig` import paths keep working.
pub use batlehub_core::ports::S3StorageConfig;

// ── Storage ───────────────────────────────────────────────────────────────────

/// Accepts both the legacy single-backend form and the new multi-backend form.
///
/// Legacy (single backend, backwards-compatible):
/// ```toml
/// [storage]
/// type = "filesystem"
/// path = "./tmp/cache"
/// ```
///
/// Multi-backend:
/// ```toml
/// [storage]
/// default = "primary"
///
/// [[storage.backends]]
/// name = "primary"
/// type = "filesystem"
/// path = "./tmp/cache"
///
/// [[storage.backends]]
/// name = "rustfs"
/// type = "s3"
/// bucket = "artifacts"
/// region = "us-east-1"
/// endpoint_url = "http://localhost:9900"
/// force_path_style = true
/// ```
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum StoragesConfig {
    /// Legacy single backend (no `default` or `backends` keys).
    Single(StorageBackendConfig),
    /// New multi-backend with explicit default selection.
    Multi(MultiStorageConfig),
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MultiStorageConfig {
    pub default: String,
    pub backends: Vec<NamedStorageConfig>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NamedStorageConfig {
    pub name: String,
    #[serde(flatten)]
    pub config: StorageBackendConfig,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StorageBackendConfig {
    Filesystem(FilesystemStorageConfig),
    S3(S3StorageConfig),
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FilesystemStorageConfig {
    pub path: String,
}
