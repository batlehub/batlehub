use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::CoreError;

/// Tracks every artifact stored in the cache: when it was first stored, when
/// it was last accessed, its size, and which registry/package/version it belongs to.
/// Used by the eviction service and cache coherence checker.
#[derive(Debug, Clone)]
pub struct ArtifactMeta {
    pub artifact_key: String,
    pub registry: String,
    pub package_name: String,
    pub version: String,
    pub size_bytes: Option<u64>,
    pub cached_at: DateTime<Utc>,
    pub last_accessed_at: DateTime<Utc>,
}

/// Named arguments for [`ArtifactCacheMeta::record_artifact`]. Replaces a long
/// positional signature whose two adjacent `Option`s (`size`, `checksum`) were
/// easy to transpose.
///
/// `checksum` is a self-computed bare SHA-256 hex digest of the stored bytes,
/// used by re-serve integrity verification. Set `None` when not computed.
pub struct ArtifactMetaRecord<'a> {
    pub key: &'a str,
    pub registry: &'a str,
    pub package_name: &'a str,
    pub version: &'a str,
    pub size: Option<u64>,
    pub checksum: Option<&'a str>,
}

/// Hot serve-path cache-coherence operations on a single artifact: record/refresh,
/// checksum lookup, access bump, per-request TTL check, and eviction of one key.
/// This is the narrow dependency of [`ProxyService`](crate::services::ProxyService)
/// and the warming service — they never enumerate the cache.
#[async_trait]
pub trait ArtifactCacheMeta: Send + Sync {
    /// Record or refresh the metadata for a newly stored artifact.
    /// Upserts: if the key already exists, updates `size`/`checksum` and resets `cached_at`.
    async fn record_artifact(&self, rec: ArtifactMetaRecord<'_>) -> Result<(), CoreError>;

    /// Fetch the stored self-computed SHA-256 hex digest for an artifact key,
    /// or `None` if the key is unknown or has no recorded checksum.
    async fn get_artifact_checksum(&self, key: &str) -> Result<Option<String>, CoreError>;

    /// Update `last_accessed_at` to now for an existing artifact record.
    /// No-ops gracefully if the key does not exist in the meta table.
    async fn touch_artifact(&self, key: &str) -> Result<(), CoreError>;

    /// Return `true` if the artifact's `cached_at` is older than `older_than`, OR if the
    /// artifact has no metadata row (unknown age → conservatively treat as expired so the
    /// caller re-fetches rather than serving a potentially stale artifact indefinitely).
    /// More efficient than `list_expired_by_ttl` for per-request TTL checks.
    async fn is_artifact_expired(
        &self,
        key: &str,
        older_than: DateTime<Utc>,
    ) -> Result<bool, CoreError>;

    /// Remove the metadata record for a key (called when an artifact is evicted).
    async fn delete_artifact_meta(&self, key: &str) -> Result<(), CoreError>;

    /// The rows this store holds for one package: the held set a
    /// synthesised listing is a projection of (RFC 0008-bis §5.1).
    ///
    /// On the write-side trait rather than [`ArtifactInventory`] because the
    /// proxy service holds this store and not the inventory, and the join
    /// is made on the request path. The default answers nothing, which is
    /// what a store that records nothing holds; the Postgres and in-memory
    /// stores answer from their rows.
    async fn list_package_artifacts(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let _ = (registry, package);
        Ok(Vec::new())
    }

    /// Every row this store holds for one registry: the held set a
    /// registry-wide document — RubyGems' compact `/versions`, a conda
    /// subdir's `repodata.json` — is a projection of (RFC 0008-bis §13.6).
    async fn list_registry_artifacts(
        &self,
        registry: &str,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let _ = registry;
        Ok(Vec::new())
    }
}

/// Inventory / eviction queries over the whole cache-meta table. The narrow
/// dependency of admin listings and the eviction service.
#[async_trait]
pub trait ArtifactInventory: Send + Sync {
    /// List all artifact metadata rows for a given registry.
    /// Pass `""` to list across all registries.
    async fn list_artifacts(&self, registry: &str) -> Result<Vec<ArtifactMeta>, CoreError>;

    /// List all artifact metadata rows, grouped by (registry, package_name),
    /// sorted by `cached_at DESC` within each group.
    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError>;

    /// List artifact keys whose `cached_at` is older than `older_than`.
    async fn list_expired_by_ttl(
        &self,
        registry: &str,
        older_than: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError>;

    /// List artifact keys whose `last_accessed_at` is older than `idle_since`.
    async fn list_idle(
        &self,
        registry: &str,
        idle_since: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError>;

    /// Return the total cached size in bytes across all artifacts for a registry.
    async fn total_size_bytes(&self, registry: &str) -> Result<u64, CoreError>;

    /// List artifacts for a registry sorted by `last_accessed_at ASC` (oldest first),
    /// up to `limit` rows. Used for LRU size-cap eviction.
    async fn list_lru(&self, registry: &str, limit: i64) -> Result<Vec<ArtifactMeta>, CoreError>;
}

/// The full artifact-meta repository: both the hot-path cache-coherence ops and
/// the inventory/eviction queries. Anything implementing both sub-traits is an
/// `ArtifactMetaRepository` automatically (blanket impl), so a concrete adapter
/// only writes the two focused `impl` blocks and existing `Arc<dyn
/// ArtifactMetaRepository>` wiring (e.g. the eviction service, which genuinely
/// needs both) keeps working unchanged.
pub trait ArtifactMetaRepository: ArtifactCacheMeta + ArtifactInventory {}

impl<T: ArtifactCacheMeta + ArtifactInventory + ?Sized> ArtifactMetaRepository for T {}
