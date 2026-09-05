pub mod advisory;
pub mod air_gap;
/// In-memory implementations of all core port traits.
///
/// These are suitable for tests, integration harnesses, and any scenario
/// that does not need persistence. All types are always compiled (no feature
/// gates) and are thread-safe via `tokio::sync::RwLock`.
///
/// Re-exported at the crate root as `batlehub_adapters::in_memory::*`.
pub mod artifact_meta;
pub mod forge;
pub mod package_repo;
pub mod readme_repo;
pub mod sbom;
pub mod security;
pub mod stats_history;
pub mod vulnerability;

// ── Domain subfolders, mirroring `batlehub_core::ports`'s auth/governance/ops/storage split ──
// (registry-domain concerns stay flat above, as `package_repo`/`artifact_meta` already did
// before this split — there was no separate `registry/` port module to mirror there.)
pub mod auth;
pub mod governance;
pub mod ops;
pub mod storage;

pub use advisory::InMemoryAdvisoryRepository;
pub use air_gap::{InMemoryBundleHistory, InMemoryMissRecorder};
pub use artifact_meta::{InMemoryArtifactMetaRepository, NoopArtifactMetaRepository};
pub use auth::login_states::InMemoryLoginStateStore;
pub use auth::user_tokens::NullUserTokenRepository;
pub use forge::{InMemoryRateLimitBudget, InMemoryRefResolutionRepository};
pub use governance::beta_channel::InMemoryBetaChannelStore;
pub use governance::grants::InMemoryGrantRepository;
pub use governance::ownership::InMemoryOwnershipStore;
pub use governance::policy::InMemoryPolicyRepository;
pub use governance::signing_keys::InMemorySigningKeyStore;
pub use governance::team_namespace::InMemoryTeamNamespaceStore;
pub use ops::quota::InMemoryQuotaRepository;
pub use ops::upstream_status::InMemoryUpstreamStatusStore;
pub use package_repo::InMemoryPackageRepository;
pub use readme_repo::{InMemoryReadmeRepository, NoopReadmeRepository};
pub use sbom::{InMemorySbomRepository, NoopSbomRepository};
pub use security::{InMemoryScanQueue, InMemoryVerdictRepository, InMemoryWorkerRegistry};
pub use stats_history::InMemoryStatsHistory;
pub use storage::backend::InMemoryStorageBackend;
pub use vulnerability::InMemoryVulnerabilityRepository;
